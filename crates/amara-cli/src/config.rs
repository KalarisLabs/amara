//! `~/.amara/config.toml`, identities, API token and daemon discovery.

use amara_agent::{
    AcceptFiles, Agent, AgentBuilder, CommandTool, EstopFrom, EstopPolicy, FilePolicy, ToolSpec,
    UdpConfig, amara_home, identity_path,
};
use amara_core::peer::PeerId;
use amara_crypto::identity::NodeIdentity;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// All fields are optional; defaults work out of the box.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub name: Option<String>,
    pub tags: Vec<String>,
    pub port: Option<u16>,
    pub multicast: Option<bool>,
    /// Static peers (`host:port`) for networks without multicast.
    pub peers: Vec<String>,
    pub relay: Option<bool>,
    /// Also run the Bluetooth LE transport (needs a build with the `ble` feature).
    pub ble: Option<bool>,
    pub api: ApiConfig,
    pub security: SecurityConfig,
    pub store: StoreConfig,
    pub files: FilesConfig,
    pub robot: RobotConfig,
    pub tools: Vec<CommandToolConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Hold messages for out-of-range peers on behalf of others (default true).
    pub custody: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RobotConfig {
    /// Who may trigger this node's emergency stop: "anyone" (default), "trusted" or "off".
    pub estop_from: Option<EstopFrom>,
    /// Program and args run on an accepted emergency stop (stop JSON on stdin).
    pub estop_command: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesConfig {
    /// Who may send files: "anyone" (default), "trusted" or "off".
    pub accept: Option<AcceptFiles>,
    /// Largest accepted file in MiB (default 32).
    pub max_mb: Option<u64>,
    /// Download directory (default ~/.amara/received/<name>).
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub bind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub require_trusted: bool,
    /// Hex PeerIds.
    pub trusted: Vec<String>,
    pub blocked: Vec<String>,
    pub calls_per_minute: Option<u32>,
}

/// A local program exposed as a mesh tool.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandToolConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
}

impl Config {
    pub fn path() -> PathBuf {
        amara_home().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
    }
}

/// Network/identity flags shared by every command that runs a node.
#[derive(Debug, Clone, clap::Args)]
pub struct NodeArgs {
    /// Agent name shown to peers (default: config `name` or hostname).
    #[arg(long, short = 'n', global = true)]
    pub name: Option<String>,
    /// LAN multicast port.
    #[arg(long, global = true)]
    pub port: Option<u16>,
    /// Static peer `host:port` (repeatable), e.g. over Tailscale/WireGuard.
    #[arg(long = "peer", global = true)]
    pub peers: Vec<String>,
    /// Disable LAN multicast discovery (static peers only).
    #[arg(long, global = true)]
    pub no_multicast: bool,
    /// Capability tag to advertise (repeatable).
    #[arg(long = "tag", global = true)]
    pub tags: Vec<String>,
    /// Also mesh over Bluetooth LE (phones, Pis, no network needed).
    #[arg(long, global = true)]
    pub ble: bool,
}

pub fn hostname() -> String {
    let raw = std::env::var("AMARA_NAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_else(|| "agent".into());
    let clean: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if clean.is_empty() {
        "agent".into()
    } else {
        clean
    }
}

fn parse_peer_ids(list: &[String], what: &str) -> Result<Vec<PeerId>> {
    list.iter()
        .map(|s| {
            PeerId::from_hex(s.trim())
                .with_context(|| format!("{what}: '{s}' is not a 16-hex-char PeerId"))
        })
        .collect()
}

fn resolve_addrs(list: &[String], default_port: u16) -> Result<Vec<SocketAddr>> {
    use std::net::ToSocketAddrs;
    let mut out = Vec::new();
    for entry in list {
        let with_port = if entry.contains(':') {
            entry.clone()
        } else {
            format!("{entry}:{default_port}")
        };
        let addrs = with_port
            .to_socket_addrs()
            .with_context(|| format!("cannot resolve peer '{entry}'"))?;
        out.extend(addrs.filter(SocketAddr::is_ipv4));
    }
    Ok(out)
}

/// Lock an identity so two processes never share a PeerId. If `name` is taken
/// by a running process, `name-2`, `name-3`, … are tried.
fn lock_identity(name: &str) -> Result<(NodeIdentity, File)> {
    for n in 1..=32 {
        let candidate = if n == 1 {
            name.to_string()
        } else {
            format!("{name}-{n}")
        };
        let path = identity_path(&candidate);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let lock_path = path.with_extension("lock");
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        if lock.try_lock().is_ok() {
            let mut id = NodeIdentity::load_or_generate(&path, &candidate)?;
            id.set_display_name(&candidate);
            return Ok((id, lock));
        }
    }
    bail!("too many running agents named '{name}'")
}

/// A running node plus the lock that reserves its identity.
pub struct RunningNode {
    pub agent: Agent,
    _lock: File,
}

pub async fn start_node(args: &NodeArgs, default_suffix: Option<&str>) -> Result<RunningNode> {
    let config = Config::load()?;
    let base = args
        .name
        .clone()
        .or(config.name.clone())
        .unwrap_or_else(|| match default_suffix {
            Some(s) => format!("{}-{s}", hostname()),
            None => hostname(),
        });
    let (identity, lock) = lock_identity(&base)?;

    let port = args
        .port
        .or(config.port)
        .unwrap_or(amara_core::DEFAULT_MESH_PORT);
    let mut peer_list = config.peers.clone();
    peer_list.extend(args.peers.iter().cloned());
    let lan = UdpConfig {
        port,
        multicast: !args.no_multicast && config.multicast.unwrap_or(true),
        static_peers: resolve_addrs(&peer_list, port)?,
        ..Default::default()
    };

    let builder_name = identity.display_name().to_string();
    let peer_id = identity.peer_id();
    let mut builder: AgentBuilder = Agent::builder(identity.display_name().to_string())
        .identity(identity)
        .lan(lan);
    for tag in config.tags.iter().chain(args.tags.iter()) {
        builder = builder.tag(tag.clone());
    }
    for peer in parse_peer_ids(&config.security.trusted, "security.trusted")? {
        builder = builder.trust(peer);
    }
    for peer in parse_peer_ids(&config.security.blocked, "security.blocked")? {
        builder = builder.block(peer);
    }
    builder = builder
        .require_trusted(config.security.require_trusted)
        .relay(config.relay.unwrap_or(true))
        .custody(config.store.custody.unwrap_or(true));
    let defaults = FilePolicy::default();
    builder = builder.files(FilePolicy {
        accept: config.files.accept.unwrap_or(defaults.accept),
        max_size: config
            .files
            .max_mb
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(defaults.max_size),
        dir: config.files.dir.clone(),
    });
    builder = builder.estop(EstopPolicy {
        from: config.robot.estop_from.unwrap_or_default(),
        command: config
            .robot
            .estop_command
            .split_first()
            .map(|(program, args)| (program.clone(), args.to_vec())),
    });
    if args.ble || config.ble.unwrap_or(false) {
        builder = with_ble(builder, &builder_name, peer_id)?;
    }
    if let Some(n) = config.security.calls_per_minute {
        builder = builder.calls_per_minute(n);
    }
    for t in &config.tools {
        let mut spec = ToolSpec::new(&t.name, &t.description);
        if let Some(schema) = &t.input_schema {
            spec = spec.with_schema(schema.clone());
        }
        let timeout = Duration::from_secs(t.timeout_secs.unwrap_or(60));
        builder = builder.tool(CommandTool::new(spec, &t.command, t.args.clone(), timeout));
    }
    let agent = builder.start().await.context("starting mesh node")?;
    Ok(RunningNode { agent, _lock: lock })
}

#[cfg(feature = "ble")]
fn with_ble(builder: AgentBuilder, name: &str, id: PeerId) -> Result<AgentBuilder> {
    use amara_transport::{BleTransport, NativeRadio, NativeRadioConfig};
    let radio = NativeRadio::new(NativeRadioConfig {
        // Short: legacy advertisements leave little room next to a 128-bit UUID.
        name: format!("am-{name}").chars().take(12).collect(),
        ..Default::default()
    });
    Ok(builder.transport(Box::new(BleTransport::new(id, radio))))
}

#[cfg(not(feature = "ble"))]
fn with_ble(_: AgentBuilder, _: &str, _: PeerId) -> Result<AgentBuilder> {
    bail!("this amara build has no Bluetooth support; rebuild with `--features ble`")
}

// ── API token & daemon discovery ─────────────────────────────────────────────

pub fn token_path() -> PathBuf {
    amara_home().join("api.token")
}

/// Read or create the local API bearer token.
pub fn api_token() -> Result<String> {
    let path = token_path();
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let token = hex::encode(bytes);
    write_private(&path, &token)?;
    Ok(token)
}

pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub url: String,
    pub name: String,
    pub id: String,
    pub pid: u32,
}

pub fn daemon_path() -> PathBuf {
    amara_home().join("daemon.json")
}

pub fn api_bind(cli: Option<&str>) -> Result<String> {
    Ok(cli
        .map(str::to_string)
        .or(Config::load()?.api.bind)
        .unwrap_or_else(|| amara_core::DEFAULT_API_BIND.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let cfg: Config = toml::from_str(
            r#"
            name = "nova"
            tags = ["reasoning"]
            port = 7500
            peers = ["10.0.0.2:7433"]
            [api]
            bind = "127.0.0.1:9000"
            [security]
            require_trusted = true
            trusted = ["0011223344556677"]
            [[tools]]
            name = "ocr"
            command = "tesseract"
            args = ["-", "-"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.name.as_deref(), Some("nova"));
        assert_eq!(cfg.tools[0].command, "tesseract");
        assert_eq!(parse_peer_ids(&cfg.security.trusted, "t").unwrap().len(), 1);
        assert!(
            toml::from_str::<Config>("bogus = 1").is_err(),
            "unknown keys are rejected"
        );
    }

    #[test]
    fn hostname_is_sanitized() {
        let h = hostname();
        assert!(!h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn peer_addresses_default_port() {
        let addrs = resolve_addrs(&["127.0.0.1".into()], 7433).unwrap();
        assert_eq!(addrs[0].port(), 7433);
        assert!(parse_peer_ids(&["xyz".into()], "t").is_err());
    }
}
