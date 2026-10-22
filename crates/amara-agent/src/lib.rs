//! # amara-agent
//!
//! Build an agent that joins the AMARA mesh in a few lines:
//!
//! ```no_run
//! use amara_agent::{Agent, ToolSpec};
//! use serde_json::json;
//!
//! # async fn demo() -> amara_core::Result<()> {
//! let agent = Agent::builder("nova")
//!     .tag("reasoning")
//!     .tool_fn(ToolSpec::new("echo", "Echo the arguments back"), |ctx| async move { Ok(ctx.args) })
//!     .start()
//!     .await?;
//!
//! // Call a tool on whichever nearby agent offers it.
//! let text = agent.call_tool(None, "summarize", json!({"text": "..."})).await?;
//! # Ok(()) }
//! ```

use amara_core::peer::PeerId;
use amara_core::{AmaraError, Result};
use amara_crypto::identity::NodeIdentity;
use amara_mesh::{MeshConfig, MeshEngine};
use amara_transport::Transport;
use serde_json::Value;
use std::future::Future;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use amara_runtime::{
    AcceptFiles, AgentRuntime, CallContext, CommandTool, Delivery, ESTOP_TOPIC, EstopFrom,
    EstopPolicy, FilePolicy, FileReceipt, FnTool, InboxItem, PeerSummary, RuntimeConfig,
    StreamSink, Tool, ToolResult, ToolSpec, TopicSample,
};
#[cfg(feature = "udp")]
pub use amara_transport::UdpConfig;

/// AMARA data directory: `$AMARA_HOME` or `~/.amara`.
pub fn amara_home() -> PathBuf {
    std::env::var_os("AMARA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".amara")))
        .unwrap_or_else(|| PathBuf::from(".amara"))
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Default identity file for an agent name.
pub fn identity_path(name: &str) -> PathBuf {
    amara_home()
        .join("identities")
        .join(format!("{}.json", safe_name(name)))
}

/// Builder for [`Agent`].
pub struct AgentBuilder {
    name: String,
    identity: Option<NodeIdentity>,
    ephemeral: bool,
    tools: Vec<Arc<dyn Tool>>,
    transports: Vec<Box<dyn Transport>>,
    #[cfg(feature = "udp")]
    lan: Option<UdpConfig>,
    mesh: MeshConfig,
    runtime: RuntimeConfig,
}

impl AgentBuilder {
    /// Advertise a capability tag (e.g. `vision`, `reasoning`).
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.runtime.tags.push(tag.into());
        self
    }

    /// Serve a tool.
    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Serve a tool backed by an async closure.
    pub fn tool_fn<F, Fut>(self, spec: ToolSpec, f: F) -> Self
    where
        F: Fn(CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        self.tool(FnTool::new(spec, f))
    }

    /// Use a specific identity instead of `~/.amara/identities/<name>.json`.
    pub fn identity(mut self, identity: NodeIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Generate a throwaway identity that is never written to disk.
    pub fn ephemeral(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    /// Add a custom transport (BLE, simulator, …). Disables the default LAN transport
    /// unless [`AgentBuilder::lan`] is also called.
    pub fn transport(mut self, transport: Box<dyn Transport>) -> Self {
        self.transports.push(transport);
        self
    }

    /// Configure the LAN transport.
    #[cfg(feature = "udp")]
    pub fn lan(mut self, config: UdpConfig) -> Self {
        self.lan = Some(config);
        self
    }

    /// Only trusted peers may call this agent's tools.
    pub fn require_trusted(mut self, yes: bool) -> Self {
        self.runtime.require_trusted = yes;
        self
    }

    pub fn trust(mut self, peer: PeerId) -> Self {
        self.mesh.trusted.insert(peer);
        self
    }

    pub fn block(mut self, peer: PeerId) -> Self {
        self.mesh.blocked.insert(peer);
        self
    }

    /// Disable relaying traffic for other peers.
    pub fn relay(mut self, enabled: bool) -> Self {
        self.mesh.relay_enabled = enabled;
        self
    }

    pub fn calls_per_minute(mut self, n: u32) -> Self {
        self.runtime.calls_per_minute = n;
        self
    }

    /// Packets accepted per second from any one source (default 200).
    pub fn packets_per_sec(mut self, n: u32) -> Self {
        self.mesh.packets_per_sec = n;
        self
    }

    /// Hold packets for out-of-range peers on behalf of others (default on).
    pub fn custody(mut self, enabled: bool) -> Self {
        self.mesh.custody = enabled;
        self
    }

    /// Persist known peers and held packets here, so store-and-forward survives
    /// restarts. Defaults to `~/.amara/state/<name>` for non-ephemeral agents.
    pub fn state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.mesh.state_dir = Some(dir.into());
        self
    }

    /// Who may trigger this agent's emergency stop, and the program it runs.
    pub fn estop(mut self, policy: EstopPolicy) -> Self {
        self.runtime.estop = policy;
        self
    }

    /// Who may send this agent files, the size limit, and where files land
    /// (default `~/.amara/received/<name>` for non-ephemeral agents).
    pub fn files(mut self, policy: FilePolicy) -> Self {
        self.runtime.files = policy;
        self
    }

    /// Join the mesh.
    pub async fn start(self) -> Result<Agent> {
        let identity = match (self.identity, self.ephemeral) {
            (Some(id), _) => id,
            (None, true) => NodeIdentity::generate(&self.name),
            (None, false) => {
                let mut id =
                    NodeIdentity::load_or_generate(&identity_path(&self.name), &self.name)?;
                id.set_display_name(&self.name);
                id
            }
        };
        let peer_id = identity.peer_id();
        let mut mesh_config = self.mesh;
        let mut runtime_config = self.runtime;
        if !self.ephemeral {
            let dir = safe_name(identity.display_name());
            mesh_config
                .state_dir
                .get_or_insert_with(|| amara_home().join("state").join(&dir));
            runtime_config
                .files
                .dir
                .get_or_insert_with(|| amara_home().join("received").join(&dir));
        }
        let mesh = Arc::new(MeshEngine::new(identity, mesh_config));

        #[cfg(feature = "udp")]
        let lan = match (self.lan, self.transports.is_empty()) {
            (Some(cfg), _) => Some(cfg),
            (None, true) => Some(UdpConfig::default()),
            (None, false) => None,
        };
        #[cfg(feature = "udp")]
        if let Some(cfg) = lan {
            mesh.add_transport(Box::new(amara_transport::UdpTransport::new(peer_id, cfg)))
                .await;
        }
        #[cfg(not(feature = "udp"))]
        if self.transports.is_empty() {
            return Err(AmaraError::NoTransport);
        }
        for t in self.transports {
            mesh.add_transport(t).await;
        }

        let runtime = AgentRuntime::new(mesh, runtime_config);
        for tool in self.tools {
            runtime.register_tool(tool)?;
        }
        runtime.start().await?;
        Ok(Agent { runtime })
    }
}

/// A running agent. Dereferences to [`AgentRuntime`] for the full API.
#[derive(Clone)]
pub struct Agent {
    runtime: Arc<AgentRuntime>,
}

impl Agent {
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder {
            name: name.into(),
            identity: None,
            ephemeral: false,
            tools: Vec::new(),
            transports: Vec::new(),
            #[cfg(feature = "udp")]
            lan: None,
            mesh: MeshConfig::default(),
            runtime: RuntimeConfig::default(),
        }
    }

    pub fn runtime(&self) -> &Arc<AgentRuntime> {
        &self.runtime
    }

    /// Call `tool` and return its data, turning a failed reply into an error.
    /// `target`: `None` (best peer offering `tool`), `"@tag"`, a name, or a hex id prefix.
    pub async fn call_tool(&self, target: Option<&str>, tool: &str, args: Value) -> Result<Value> {
        let peer = self.runtime.resolve_target(target, tool)?;
        let reply = self.runtime.call(peer.peer_id, tool, args, None).await?;
        if reply.ok {
            Ok(reply.data)
        } else {
            Err(AmaraError::Other(
                reply
                    .data
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("call failed")
                    .to_string(),
            ))
        }
    }

    /// Wait until at least `n` peers are known (or the timeout passes). Returns the count.
    pub async fn wait_for_peers(&self, n: usize, timeout: Duration) -> usize {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let count = self.runtime.peers().len();
            if count >= n || tokio::time::Instant::now() >= deadline {
                return count;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Deref for Agent {
    type Target = AgentRuntime;
    fn deref(&self) -> &AgentRuntime {
        &self.runtime
    }
}
