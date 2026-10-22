//! The agent runtime: tools, calls, messages and presence on top of the mesh.
//!
//! Every directed payload (calls, replies, chunks, files, direct messages) is
//! sealed end-to-end to the recipient's X25519 key, so relays only ever see the
//! routing header. Broadcast chat is signed but public, like a BitChat channel.

use crate::files::{FilePolicy, IncomingFile};
use crate::inbox::{Inbox, InboxItem};
use crate::tool::{CallContext, StreamSink, Tool, ToolResult, ToolSpec, validate_tool_name};
use crate::topics::{EstopPolicy, Topics};
use amara_core::capability::{Availability, LoadClass};
use amara_core::constants::{
    AMARA_VERSION, ANNOUNCE_INTERVAL_SECS, DEFAULT_CALL_TIMEOUT_SECS, DEFAULT_TTL, MAX_PACKET_SIZE,
    RATE_LIMIT_TASKS_PER_MIN,
};
use amara_core::peer::{PeerId, PeerInfo, Platform, TrustLevel};
use amara_core::types::{AmaraEvent, now_timestamp};
use amara_core::{AmaraError, Result};
use amara_crypto::identity::NodeIdentity;
use amara_crypto::sealed;
use amara_mesh::{Delivery, Inbound, MeshEngine};
use amara_protocol::codec::Announce;
use amara_protocol::header::PacketHeader;
use amara_protocol::messages::{decode_body, encode_body};
use amara_protocol::{Call, Chat, Chunk, Packet, PacketFlags, PacketType, Reply};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Maximum time a locally executed tool may run.
const MAX_TOOL_RUNTIME: Duration = Duration::from_secs(300);
/// How long a caller waits for chunks that were overtaken by the final reply.
const LATE_CHUNK_GRACE: Duration = Duration::from_secs(2);

/// Runtime policy.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Capability tags advertised besides tool names (e.g. `reasoning`, `vision`).
    pub tags: Vec<String>,
    /// Only peers marked trusted may call tools.
    pub require_trusted: bool,
    /// Incoming calls allowed per peer per minute.
    pub calls_per_minute: u32,
    /// Which incoming files are accepted, and where they are saved.
    pub files: FilePolicy,
    /// Who may trigger this node's emergency stop, and what it runs.
    pub estop: EstopPolicy,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            require_trusted: false,
            calls_per_minute: RATE_LIMIT_TASKS_PER_MIN,
            files: FilePolicy::default(),
            estop: EstopPolicy::default(),
        }
    }
}

enum ToolEntry {
    Local(Arc<dyn Tool>),
    /// Served by an external agent through the inbox.
    Inbox(ToolSpec),
}

impl ToolEntry {
    fn spec(&self) -> ToolSpec {
        match self {
            Self::Local(t) => t.spec(),
            Self::Inbox(s) => s.clone(),
        }
    }
}

struct IncomingCall {
    caller: PeerId,
    started: Instant,
    reply_tx: Option<oneshot::Sender<ToolResult>>,
    stream: StreamSink,
}

/// Re-orders the chunks of one streaming call we made.
struct StreamState {
    from: PeerId,
    next: u32,
    early: BTreeMap<u32, Value>,
    tx: mpsc::UnboundedSender<Value>,
}

impl StreamState {
    fn push(&mut self, seq: u32, data: Value) {
        if seq < self.next {
            return; // duplicate
        }
        self.early.insert(seq, data);
        while let Some(data) = self.early.remove(&self.next) {
            let _ = self.tx.send(data);
            self.next += 1;
        }
    }

    /// Deliver whatever is left, in order, skipping gaps.
    fn flush(&mut self) {
        for (seq, data) in std::mem::take(&mut self.early) {
            let _ = self.tx.send(data);
            self.next = seq + 1;
        }
    }
}

/// Compact peer view for UIs and LLM tools.
#[derive(Debug, Clone, Serialize)]
pub struct PeerSummary {
    pub id: String,
    pub name: String,
    pub hops: u8,
    pub capabilities: Vec<String>,
    pub platform: &'static str,
    pub load: LoadClass,
    pub trust: TrustLevel,
    pub last_seen_secs: i64,
}

impl From<&PeerInfo> for PeerSummary {
    fn from(p: &PeerInfo) -> Self {
        Self {
            id: p.peer_id.to_string(),
            name: p.display_name.clone(),
            hops: p.hop_count,
            capabilities: p.capabilities.clone(),
            platform: p.platform.display_name(),
            load: p.load_class,
            trust: p.trust_level,
            last_seen_secs: (chrono::Utc::now() - p.last_seen).num_seconds().max(0),
        }
    }
}

/// The agent runtime.
pub struct AgentRuntime {
    pub(crate) mesh: Arc<MeshEngine>,
    pub(crate) config: RuntimeConfig,
    x25519: x25519_dalek::StaticSecret,
    tools: DashMap<String, ToolEntry>,
    pending_out: DashMap<Uuid, oneshot::Sender<Reply>>,
    pending_in: DashMap<Uuid, IncomingCall>,
    pending_streams: DashMap<Uuid, StreamState>,
    pub(crate) files_out:
        DashMap<Uuid, (PeerId, mpsc::UnboundedSender<amara_protocol::FileStatus>)>,
    pub(crate) files_in: DashMap<Uuid, IncomingFile>,
    pub(crate) files_done: DashMap<Uuid, (PeerId, Instant)>,
    call_rate: DashMap<PeerId, (Instant, u32)>,
    in_flight: AtomicUsize,
    pub(crate) inbox: Inbox,
    pub(crate) topics: Topics,
    announce_now: Notify,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AgentRuntime {
    pub fn new(mesh: Arc<MeshEngine>, config: RuntimeConfig) -> Arc<Self> {
        let x25519 = mesh.identity().x25519_secret();
        Arc::new(Self {
            mesh,
            config,
            x25519,
            tools: DashMap::new(),
            pending_out: DashMap::new(),
            pending_in: DashMap::new(),
            pending_streams: DashMap::new(),
            files_out: DashMap::new(),
            files_in: DashMap::new(),
            files_done: DashMap::new(),
            call_rate: DashMap::new(),
            in_flight: AtomicUsize::new(0),
            inbox: Inbox::default(),
            topics: Topics::default(),
            announce_now: Notify::new(),
            tasks: Mutex::new(Vec::new()),
        })
    }

    pub fn identity(&self) -> &NodeIdentity {
        self.mesh.identity()
    }

    pub fn mesh(&self) -> &Arc<MeshEngine> {
        &self.mesh
    }

    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Mesh lifecycle/security events.
    pub fn events(&self) -> broadcast::Receiver<AmaraEvent> {
        self.mesh.events()
    }

    // ── lifecycle ────────────────────────────────────────────────────────────

    /// Start the mesh, the packet loop and periodic presence announcements.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.mesh.start().await?;
        let rx = self.mesh.subscribe();
        let rt = self.clone();
        let run = tokio::spawn(async move { rt.run(rx).await });
        let rt = self.clone();
        let announce = tokio::spawn(async move { rt.announce_loop().await });
        let rt = self.clone();
        let watch = tokio::spawn(async move { rt.watch_peers().await });
        let rt = self.clone();
        let janitor = tokio::spawn(async move { rt.file_janitor().await });
        self.tasks
            .lock()
            .unwrap()
            .extend([run, announce, watch, janitor]);
        Ok(())
    }

    /// Stop background work.
    pub fn shutdown(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        self.mesh.shutdown();
    }

    async fn announce_loop(self: Arc<Self>) {
        let link_up = self.mesh.link_up();
        loop {
            if let Err(e) = self.announce().await {
                debug!("announce failed: {e}");
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(ANNOUNCE_INTERVAL_SECS)) => {}
                _ = self.announce_now.notified() => {}
                _ = link_up.notified() => {}
            }
            // Coalesce bursts (several peers appearing at once).
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    /// Re-announce whenever a new peer appears so it learns about us immediately.
    async fn watch_peers(self: Arc<Self>) {
        let mut events = self.mesh.events();
        loop {
            match events.recv().await {
                Ok(AmaraEvent::PeerJoined { .. }) => self.announce_now.notify_one(),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    fn load_class(&self) -> LoadClass {
        match self.in_flight.load(Ordering::Relaxed) {
            0 => LoadClass::Idle,
            1..=2 => LoadClass::Low,
            3..=5 => LoadClass::Medium,
            6..=10 => LoadClass::High,
            _ => LoadClass::Overloaded,
        }
    }

    /// Capability tags plus tool names, as advertised in announces.
    pub fn capabilities(&self) -> Vec<String> {
        let mut caps = self.config.tags.clone();
        let mut tools: Vec<String> = self.tools.iter().map(|t| t.key().clone()).collect();
        tools.sort();
        for t in tools {
            if !caps.contains(&t) {
                caps.push(t);
            }
        }
        caps
    }

    /// Broadcast presence (ANNOUNCE).
    pub async fn announce(&self) -> Result<()> {
        let id = self.identity();
        let announce = Announce {
            public_key: id.public_key_bytes(),
            x25519_public: id.x25519_public().to_bytes(),
            name: id.display_name().to_string(),
            version: AMARA_VERSION.to_string(),
            platform: Platform::current(),
            load: self.load_class(),
            availability: Availability::Online,
            capabilities: self.capabilities(),
        };
        let packet = Packet::broadcast(
            PacketType::Announce,
            id.peer_id(),
            DEFAULT_TTL,
            announce.encode(),
        );
        self.mesh.send_packet(packet).await
    }

    // ── tools ────────────────────────────────────────────────────────────────

    /// Register a locally executed tool.
    pub fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        let name = tool.spec().name;
        validate_tool_name(&name).map_err(AmaraError::Config)?;
        self.tools.insert(name.clone(), ToolEntry::Local(tool));
        info!("tool registered: {name}");
        self.announce_now.notify_one();
        Ok(())
    }

    /// Expose a tool answered by an external agent through the inbox + [`AgentRuntime::reply`].
    pub fn expose(&self, spec: ToolSpec) -> Result<()> {
        validate_tool_name(&spec.name).map_err(AmaraError::Config)?;
        let name = spec.name.clone();
        self.tools.insert(name.clone(), ToolEntry::Inbox(spec));
        info!("tool exposed via inbox: {name}");
        self.announce_now.notify_one();
        Ok(())
    }

    /// Stop serving a tool. Returns true if it existed.
    pub fn remove_tool(&self, name: &str) -> bool {
        let removed = self.tools.remove(name).is_some();
        if removed {
            self.announce_now.notify_one();
        }
        removed
    }

    /// Specs of all tools this agent serves.
    pub fn tools(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self.tools.iter().map(|t| t.value().spec()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// This agent's card: identity plus full tool schemas (served as `amara.card`).
    pub fn card(&self) -> Value {
        let id = self.identity();
        json!({
            "name": id.display_name(),
            "id": id.peer_id().to_string(),
            "version": AMARA_VERSION,
            "platform": Platform::current().display_name(),
            "tags": self.config.tags,
            "tools": self.tools(),
        })
    }

    // ── peers ────────────────────────────────────────────────────────────────

    pub fn peers(&self) -> Vec<PeerSummary> {
        let mut peers: Vec<PeerSummary> = self
            .mesh
            .registry()
            .all_peers()
            .iter()
            .map(PeerSummary::from)
            .collect();
        peers.sort_by(|a, b| a.hops.cmp(&b.hops).then_with(|| a.name.cmp(&b.name)));
        peers
    }

    /// Resolve a peer by name, name prefix or hex id prefix.
    pub fn resolve(&self, query: &str) -> Result<PeerInfo> {
        self.mesh
            .registry()
            .resolve(query)
            .ok_or_else(|| AmaraError::UnknownPeer(format!("no unique peer matches '{query}'")))
    }

    /// Resolve a peer that may be out of range: nearby peers first, then every
    /// peer met before. Returns the PeerId and display name.
    pub fn resolve_any(&self, query: &str) -> Result<(PeerId, String)> {
        if let Some(p) = self.mesh.registry().resolve(query) {
            return Ok((p.peer_id, p.display_name));
        }
        let q = query.trim().trim_start_matches('@').to_lowercase();
        let mut known = self.mesh.known_peers();
        known.retain(|(id, _)| *id != self.identity().peer_id());
        known.sort_by_key(|(_, p)| std::cmp::Reverse(p.last_seen));
        let unique = |matches: Vec<&(PeerId, amara_mesh::KnownPeer)>| match matches.as_slice() {
            [one] => Some((one.0, one.1.name.clone())),
            _ => None,
        };
        let found = (q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| {
                unique(
                    known
                        .iter()
                        .filter(|(id, _)| id.to_string().starts_with(&q))
                        .collect(),
                )
            })
            .flatten()
            // Names are not unique; the most recently seen holder wins.
            .or_else(|| {
                known
                    .iter()
                    .find(|(_, p)| p.name.to_lowercase() == q)
                    .map(|(id, p)| (*id, p.name.clone()))
            })
            .or_else(|| {
                unique(
                    known
                        .iter()
                        .filter(|(_, p)| p.name.to_lowercase().starts_with(&q))
                        .collect(),
                )
            });
        found.ok_or_else(|| {
            AmaraError::UnknownPeer(format!("no peer matches '{query}' (nearby or met before)"))
        })
    }

    /// Pick the best peer serving `capability` (tool name or tag): fewest hops, then lowest load.
    pub fn best_peer_for(&self, capability: &str) -> Result<PeerInfo> {
        let mut candidates = self.mesh.registry().find_by_capability(capability);
        candidates.retain(|p| p.trust_level != TrustLevel::Blocked);
        candidates.sort_by_key(|p| (p.hop_count, p.load_class.to_u8(), -(p.trust_level as i32)));
        candidates.into_iter().next().ok_or_else(|| {
            AmaraError::CapabilityNotAvailable(format!("no nearby agent offers '{capability}'"))
        })
    }

    /// Resolve a call target: `None` → best peer offering `tool`; `"@tag"` → best peer
    /// with that capability; anything else → a specific peer.
    pub fn resolve_target(&self, target: Option<&str>, tool: &str) -> Result<PeerInfo> {
        match target.map(str::trim).filter(|t| !t.is_empty()) {
            None => self.best_peer_for(tool),
            Some(t) if t.starts_with('@') && self.mesh.registry().resolve(t).is_none() => {
                self.best_peer_for(&t[1..])
            }
            Some(t) => self.resolve(t),
        }
    }

    // ── sending ──────────────────────────────────────────────────────────────

    fn aad(header: &PacketHeader) -> Vec<u8> {
        let mut aad = Vec::with_capacity(25);
        aad.push(header.packet_type as u8);
        aad.extend_from_slice(&header.message_id);
        aad.extend_from_slice(header.source_id.as_bytes());
        aad.extend_from_slice(header.destination_id.as_bytes());
        aad
    }

    /// Build a packet carrying `bytes`, sealed to `dest` unless it is a broadcast.
    /// The recipient's key comes from the registry, or from peers met before.
    pub(crate) fn seal_packet(
        &self,
        packet_type: PacketType,
        dest: PeerId,
        bytes: Vec<u8>,
        compressed: bool,
    ) -> Result<Packet> {
        let mut packet = Packet::new(
            packet_type,
            self.identity().peer_id(),
            dest,
            DEFAULT_TTL,
            Vec::new(),
        );
        if compressed {
            packet.header.flags.set(PacketFlags::COMPRESSED);
        }
        packet.payload = if dest.is_broadcast() {
            bytes
        } else {
            let x25519 = self
                .mesh
                .registry()
                .get_peer(&dest)
                .map(|p| p.x25519_public)
                .or_else(|| self.mesh.known_peer(&dest).map(|p| p.x25519))
                .ok_or_else(|| AmaraError::UnknownPeer(dest.to_string()))?;
            packet.header.flags.set(PacketFlags::ENCRYPTED);
            sealed::seal(&x25519, &bytes, &Self::aad(&packet.header))?
        };
        if packet.payload.len() > MAX_PACKET_SIZE {
            return Err(AmaraError::PacketTooLarge {
                size: packet.payload.len(),
                max: MAX_PACKET_SIZE,
            });
        }
        Ok(packet)
    }

    pub(crate) async fn send_body<T: Serialize>(
        &self,
        packet_type: PacketType,
        dest: PeerId,
        body: &T,
    ) -> Result<()> {
        let (bytes, compressed) = encode_body(body)?;
        let packet = self.seal_packet(packet_type, dest, bytes, compressed)?;
        self.mesh.send_packet(packet).await
    }

    /// Decrypt (if sealed) a packet's payload. Directed packets must be sealed.
    pub(crate) fn open_raw(&self, packet: &Packet) -> Result<Vec<u8>> {
        if packet.header.flags.is_encrypted() {
            sealed::open(&self.x25519, &packet.payload, &Self::aad(&packet.header))
        } else if !packet.header.destination_id.is_broadcast() {
            Err(AmaraError::PermissionDenied(
                "unencrypted direct packet".into(),
            ))
        } else {
            Ok(packet.payload.clone())
        }
    }

    pub(crate) fn open_body<T: serde::de::DeserializeOwned>(&self, packet: &Packet) -> Result<T> {
        decode_body(&self.open_raw(packet)?, packet.header.flags.is_compressed())
    }

    /// Send a direct (end-to-end encrypted) message. If the recipient is out of
    /// range but has been met before, the message is stored and delivered when
    /// it comes back.
    pub async fn send_message(&self, to: PeerId, text: impl Into<String>) -> Result<Delivery> {
        let (bytes, compressed) = encode_body(&Chat {
            text: text.into(),
            channel: None,
        })?;
        let packet = self.seal_packet(PacketType::Message, to, bytes, compressed)?;
        self.mesh.send_or_store(packet).await
    }

    /// Broadcast a signed public message on an optional channel.
    pub async fn broadcast_message(
        &self,
        text: impl Into<String>,
        channel: Option<String>,
    ) -> Result<()> {
        self.send_body(
            PacketType::Message,
            PeerId::broadcast(),
            &Chat {
                text: text.into(),
                channel,
            },
        )
        .await
    }

    /// Call `tool` on `target` and wait for the reply.
    pub async fn call(
        &self,
        target: PeerId,
        tool: &str,
        args: Value,
        timeout: Option<Duration>,
    ) -> Result<Reply> {
        self.call_inner(target, tool, args, timeout, None).await
    }

    /// Call `tool` on `target`, receiving partial output on `chunks` (in order)
    /// while it runs, then the final reply.
    pub async fn call_streaming(
        &self,
        target: PeerId,
        tool: &str,
        args: Value,
        timeout: Option<Duration>,
        chunks: mpsc::UnboundedSender<Value>,
    ) -> Result<Reply> {
        self.call_inner(target, tool, args, timeout, Some(chunks))
            .await
    }

    async fn call_inner(
        &self,
        target: PeerId,
        tool: &str,
        args: Value,
        timeout: Option<Duration>,
        chunks: Option<mpsc::UnboundedSender<Value>>,
    ) -> Result<Reply> {
        let timeout = timeout.unwrap_or(Duration::from_secs(DEFAULT_CALL_TIMEOUT_SECS));
        if target == self.identity().peer_id() {
            let sink = chunks.map(StreamSink::new).unwrap_or_default();
            return self.call_self(tool, args, timeout, sink).await;
        }
        let id = Uuid::new_v4();
        let call = Call {
            id,
            tool: tool.to_string(),
            args,
            timeout_ms: timeout.as_millis().min(u32::MAX as u128) as u32,
            stream: chunks.is_some(),
        };
        if let Some(tx) = chunks {
            self.pending_streams.insert(
                id,
                StreamState {
                    from: target,
                    next: 0,
                    early: BTreeMap::new(),
                    tx,
                },
            );
        }
        let (tx, rx) = oneshot::channel();
        self.pending_out.insert(id, tx);
        let outcome = match self.send_body(PacketType::Task, target, &call).await {
            Err(e) => Err(e),
            Ok(()) => match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(reply)) => Ok(reply),
                Ok(Err(_)) => Err(AmaraError::Other("call cancelled".into())),
                Err(_) => Err(AmaraError::Timeout(format!(
                    "{tool} on {target} after {}s",
                    timeout.as_secs()
                ))),
            },
        };
        self.pending_out.remove(&id);
        if let Ok(reply) = &outcome
            && reply.chunks > 0
        {
            // The reply can overtake its last chunks on a multi-path mesh.
            let deadline = Instant::now() + LATE_CHUNK_GRACE;
            while Instant::now() < deadline
                && self
                    .pending_streams
                    .get(&id)
                    .is_some_and(|s| s.next < reply.chunks)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        if let Some((_, mut stream)) = self.pending_streams.remove(&id) {
            stream.flush();
        }
        outcome
    }

    async fn call_self(
        &self,
        tool: &str,
        args: Value,
        timeout: Duration,
        stream: StreamSink,
    ) -> Result<Reply> {
        let started = Instant::now();
        let call_id = Uuid::new_v4();
        let ctx = CallContext {
            call_id,
            caller: self.identity().peer_id(),
            caller_name: self.identity().display_name().to_string(),
            args,
            stream,
        };
        let outcome = tokio::time::timeout(timeout, self.execute(tool, ctx))
            .await
            .unwrap_or_else(|_| Err("timed out".into()));
        self.pending_in.remove(&call_id);
        Ok(to_reply(call_id, outcome, started, 0))
    }

    /// Answer a call that arrived through the inbox.
    pub fn reply(&self, call_id: Uuid, outcome: ToolResult) -> Result<()> {
        let (_, mut pending) = self.pending_in.remove(&call_id).ok_or_else(|| {
            AmaraError::TaskNotFound(format!("{call_id} (already answered or expired)"))
        })?;
        if let Some(tx) = pending.reply_tx.take() {
            let _ = tx.send(outcome);
        }
        Ok(())
    }

    /// Send partial output for a streaming call that arrived through the inbox.
    /// Finish it with [`AgentRuntime::reply`].
    pub fn reply_partial(&self, call_id: Uuid, data: Value) -> Result<()> {
        let pending = self.pending_in.get(&call_id).ok_or_else(|| {
            AmaraError::TaskNotFound(format!("{call_id} (already answered or expired)"))
        })?;
        if !pending.stream.send(data) {
            return Err(AmaraError::Other(
                "the caller did not ask for partial results; send the final reply".into(),
            ));
        }
        Ok(())
    }

    // ── receiving ────────────────────────────────────────────────────────────

    async fn run(self: Arc<Self>, mut rx: broadcast::Receiver<Inbound>) {
        loop {
            let inbound = match rx.recv().await {
                Ok(i) => i,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("runtime lagged, {n} packets skipped");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match inbound.packet.header.packet_type {
                PacketType::Task => {
                    let rt = self.clone();
                    tokio::spawn(async move { rt.handle_call(inbound).await });
                }
                PacketType::Result => match self.open_body::<Reply>(&inbound.packet) {
                    Ok(reply) => {
                        if let Some((_, tx)) = self.pending_out.remove(&reply.id) {
                            let _ = tx.send(reply);
                        }
                    }
                    Err(e) => debug!("bad reply: {e}"),
                },
                PacketType::ResultChunk => match self.open_body::<Chunk>(&inbound.packet) {
                    Ok(chunk) => {
                        if let Some(mut stream) = self.pending_streams.get_mut(&chunk.id)
                            && stream.from == inbound.packet.header.source_id
                        {
                            stream.push(chunk.seq, chunk.data);
                        }
                    }
                    Err(e) => debug!("bad chunk: {e}"),
                },
                PacketType::Message => match self.open_body::<Chat>(&inbound.packet) {
                    Ok(chat) => self.inbox.push(InboxItem::Message {
                        from: inbound.packet.header.source_id.to_string(),
                        from_name: inbound.sender_name,
                        text: chat.text,
                        channel: chat.channel,
                        direct: inbound.packet.header.flags.is_encrypted(),
                        at: inbound.packet.header.timestamp,
                        stored: inbound.stored,
                    }),
                    Err(e) => debug!("bad message: {e}"),
                },
                PacketType::Event => {
                    if let Err(e) = self.handle_sample(inbound) {
                        debug!("bad sample: {e}");
                    }
                }
                PacketType::FileManifest | PacketType::FileChunk | PacketType::FileComplete => {
                    // Handled in order: a transfer's chunks must land before its completion.
                    if let Err(e) = self.handle_file_packet(&inbound).await {
                        debug!("file packet from {}: {e}", inbound.sender_name);
                    }
                }
                _ => {}
            }
        }
    }

    fn check_policy(&self, caller: PeerId) -> std::result::Result<(), String> {
        let trust = self
            .mesh
            .registry()
            .get_peer(&caller)
            .map(|p| p.trust_level)
            .unwrap_or_default();
        if trust == TrustLevel::Blocked {
            return Err("permission denied".into());
        }
        if self.config.require_trusted && trust < TrustLevel::Trusted {
            return Err("permission denied: this agent only serves trusted peers".into());
        }
        let mut rate = self.call_rate.entry(caller).or_insert((Instant::now(), 0));
        if rate.0.elapsed() >= Duration::from_secs(60) {
            *rate = (Instant::now(), 0);
        }
        rate.1 += 1;
        if rate.1 > self.config.calls_per_minute {
            return Err("rate limited".into());
        }
        Ok(())
    }

    /// Relay a streaming call's partial output to the caller as sealed chunks.
    fn spawn_chunk_sender(
        self: &Arc<Self>,
        caller: PeerId,
        id: Uuid,
        mut rx: mpsc::UnboundedReceiver<Value>,
        sent: Arc<AtomicU32>,
    ) -> JoinHandle<()> {
        let rt = self.clone();
        tokio::spawn(async move {
            let mut seq = 0u32;
            while let Some(data) = rx.recv().await {
                if let Err(e) = rt
                    .send_body(PacketType::ResultChunk, caller, &Chunk { id, seq, data })
                    .await
                {
                    debug!("chunk {seq} to {caller} failed: {e}");
                }
                seq += 1;
                sent.store(seq, Ordering::Release);
            }
        })
    }

    async fn handle_call(self: Arc<Self>, inbound: Inbound) {
        let caller = inbound.packet.header.source_id;
        if inbound.packet.header.destination_id != self.identity().peer_id() {
            return; // calls are always directed
        }
        let call: Call = match self.open_body(&inbound.packet) {
            Ok(c) => c,
            Err(e) => return debug!("bad call from {caller}: {e}"),
        };
        let started = Instant::now();
        let sent = Arc::new(AtomicU32::new(0));
        let mut chunk_sender = None;
        let outcome = match self.check_policy(caller) {
            Err(e) => Err(e),
            Ok(()) => {
                self.in_flight.fetch_add(1, Ordering::Relaxed);
                let budget = match call.timeout_ms {
                    0 => MAX_TOOL_RUNTIME,
                    ms => Duration::from_millis(ms as u64).min(MAX_TOOL_RUNTIME),
                };
                let stream = if call.stream {
                    let (tx, rx) = mpsc::unbounded_channel();
                    chunk_sender = Some(self.spawn_chunk_sender(caller, call.id, rx, sent.clone()));
                    StreamSink::new(tx)
                } else {
                    StreamSink::default()
                };
                let ctx = CallContext {
                    call_id: call.id,
                    caller,
                    caller_name: inbound.sender_name,
                    args: call.args,
                    stream,
                };
                let outcome = tokio::time::timeout(budget, self.execute(&call.tool, ctx))
                    .await
                    .unwrap_or_else(|_| Err(format!("{} timed out", call.tool)));
                self.pending_in.remove(&call.id);
                self.in_flight.fetch_sub(1, Ordering::Relaxed);
                outcome
            }
        };
        if let Some(mut sender) = chunk_sender {
            // Every sink clone is gone once the tool returns, unless the tool leaked
            // one into a background task; don't wait forever for that.
            if tokio::time::timeout(Duration::from_secs(5), &mut sender)
                .await
                .is_err()
            {
                sender.abort();
            }
        }
        let reply = to_reply(call.id, outcome, started, sent.load(Ordering::Acquire));
        if let Err(e) = self.send_body(PacketType::Result, caller, &reply).await {
            warn!("failed to send reply to {caller}: {e}");
        }
    }

    /// Run a tool (built-in, local or inbox-served).
    async fn execute(&self, tool: &str, ctx: CallContext) -> ToolResult {
        match tool {
            "amara.card" => return Ok(self.card()),
            "amara.ping" => return Ok(json!({ "pong": now_timestamp() })),
            _ => {}
        }
        enum Kind {
            Local(Arc<dyn Tool>),
            Inbox,
        }
        let kind = match self.tools.get(tool).as_deref() {
            Some(ToolEntry::Local(t)) => Kind::Local(t.clone()),
            Some(ToolEntry::Inbox(_)) => Kind::Inbox,
            None => return Err(format!("unknown tool '{tool}'")),
        };
        match kind {
            Kind::Local(t) => t.call(ctx).await,
            Kind::Inbox => {
                let (tx, rx) = oneshot::channel();
                let call_id = ctx.call_id;
                let stream = ctx.stream.is_active();
                self.pending_in.insert(
                    call_id,
                    IncomingCall {
                        caller: ctx.caller,
                        started: Instant::now(),
                        reply_tx: Some(tx),
                        stream: ctx.stream,
                    },
                );
                self.inbox.push(InboxItem::Call {
                    call_id,
                    from: ctx.caller.to_string(),
                    from_name: ctx.caller_name,
                    tool: tool.to_string(),
                    args: ctx.args,
                    at: now_timestamp(),
                    expires_in: MAX_TOOL_RUNTIME.as_secs(),
                    stream,
                });
                let outcome = rx
                    .await
                    .unwrap_or_else(|_| Err("agent did not answer".into()));
                self.pending_in.remove(&call_id);
                outcome
            }
        }
    }

    /// Calls waiting for an inbox reply: (call id, caller, seconds waiting).
    pub fn pending_calls(&self) -> Vec<(Uuid, PeerId, u64)> {
        self.pending_in
            .iter()
            .map(|e| (*e.key(), e.caller, e.started.elapsed().as_secs()))
            .collect()
    }
}

impl Drop for AgentRuntime {
    fn drop(&mut self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

fn to_reply(id: Uuid, outcome: ToolResult, started: Instant, chunks: u32) -> Reply {
    let ms = started.elapsed().as_millis() as u64;
    let (ok, data) = match outcome {
        Ok(data) => (true, data),
        Err(error) => (false, json!({ "error": error })),
    };
    Reply {
        id,
        ok,
        data,
        ms,
        chunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_state_reorders_and_flushes() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = StreamState {
            from: PeerId::from_bytes([1; 8]),
            next: 0,
            early: BTreeMap::new(),
            tx,
        };
        s.push(1, json!(1));
        assert!(rx.try_recv().is_err(), "held until 0 arrives");
        s.push(0, json!(0));
        s.push(0, json!("dup"));
        s.push(3, json!(3));
        s.flush();
        let got: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(got, vec![json!(0), json!(1), json!(3)]);
    }
}
