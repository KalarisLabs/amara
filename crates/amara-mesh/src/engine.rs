//! The mesh engine: authenticated flooding with learned unicast routes.
//!
//! Inbound pipeline for every packet:
//!
//! ```text
//! frame ─► reassemble ─► decode ─► dedup ─► authenticate ─► rate-limit ─┬─► deliver (to us / broadcast)
//!                                                                       └─► relay (TTL, jitter, routes)
//! ```
//!
//! * **Authentication**: every packet must be Ed25519-signed. ANNOUNCE packets
//!   are self-certifying (PeerId = BLAKE3(public key)); all other packets are
//!   verified against the key learned from that peer's announce.
//! * **Routing**: announces flood with TTL. The neighbor that delivered the
//!   lowest-hop copy becomes the next hop for unicast traffic to that peer, so
//!   directed packets travel one path instead of flooding. Unknown routes fall
//!   back to flooding.

use crate::dedup::DedupCache;
use crate::registry::PeerRegistry;
use amara_core::constants::{
    ANNOUNCE_INTERVAL_SECS, DEFAULT_TTL, MAX_CLOCK_SKEW_SECS, PEER_EXPIRY_SECS,
    RATE_LIMIT_PACKETS_PER_SEC, RELAY_JITTER_MAX_MS, RELAY_JITTER_MIN_MS,
};
use amara_core::peer::{PeerId, PeerInfo, TrustLevel};
use amara_core::types::{AmaraEvent, now_timestamp};
use amara_core::{AmaraError, Result};
use amara_crypto::identity::NodeIdentity;
use amara_protocol::codec::Announce;
use amara_protocol::{FrameAssembler, Packet, PacketType, encode_frames};
use amara_transport::{Transport, TransportEvent};
use dashmap::DashMap;
use rand::Rng;
use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, RwLock, broadcast};
use tokio::task::JoinHandle;
use tracing::{debug, info, trace, warn};

/// Mesh behaviour switches.
#[derive(Clone, Debug)]
pub struct MeshConfig {
    pub max_ttl: u8,
    pub relay_enabled: bool,
    /// Peers (by PeerId) whose traffic is always dropped.
    pub blocked: HashSet<PeerId>,
    /// Peers (by PeerId) promoted to `TrustLevel::Trusted` when they announce.
    pub trusted: HashSet<PeerId>,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            max_ttl: DEFAULT_TTL,
            relay_enabled: true,
            blocked: HashSet::new(),
            trusted: HashSet::new(),
        }
    }
}

/// A packet delivered to this node, already authenticated.
#[derive(Debug, Clone)]
pub struct Inbound {
    pub packet: Packet,
    /// Display name of the sender at the time of delivery.
    pub sender_name: String,
}

#[derive(Debug, Clone, Copy)]
struct Route {
    via: PeerId,
    transport: usize,
    hops: u8,
    updated: Instant,
}

/// The core mesh routing engine.
pub struct MeshEngine {
    identity: NodeIdentity,
    config: MeshConfig,
    registry: PeerRegistry,
    dedup: DedupCache,
    pending: Mutex<Vec<Box<dyn Transport>>>,
    transports: RwLock<Vec<Arc<dyn Transport>>>,
    routes: DashMap<PeerId, Route>,
    rate: DashMap<PeerId, (Instant, u32)>,
    packet_tx: broadcast::Sender<Inbound>,
    event_tx: broadcast::Sender<AmaraEvent>,
    link_up: Arc<Notify>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl MeshEngine {
    pub fn new(identity: NodeIdentity, config: MeshConfig) -> Self {
        let (packet_tx, _) = broadcast::channel(4096);
        let (event_tx, _) = broadcast::channel(1024);
        Self {
            identity,
            config,
            registry: PeerRegistry::new(),
            dedup: DedupCache::new(),
            pending: Mutex::new(Vec::new()),
            transports: RwLock::new(Vec::new()),
            routes: DashMap::new(),
            rate: DashMap::new(),
            packet_tx,
            event_tx,
            link_up: Arc::new(Notify::new()),
            tasks: StdMutex::new(Vec::new()),
        }
    }

    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    pub fn registry(&self) -> &PeerRegistry {
        &self.registry
    }

    /// Queue a transport; it is started by [`MeshEngine::start`].
    pub async fn add_transport(&self, transport: Box<dyn Transport>) {
        self.pending.lock().await.push(transport);
    }

    /// Authenticated packets addressed to us (or broadcast).
    pub fn subscribe(&self) -> broadcast::Receiver<Inbound> {
        self.packet_tx.subscribe()
    }

    /// Peer lifecycle and security events.
    pub fn events(&self) -> broadcast::Receiver<AmaraEvent> {
        self.event_tx.subscribe()
    }

    /// Signalled whenever a transport discovers a new link-level neighbor, so the
    /// runtime can announce immediately instead of waiting for the next beacon.
    pub fn link_up(&self) -> Arc<Notify> {
        self.link_up.clone()
    }

    /// Hop count to a peer, if a route is known.
    pub fn hops_to(&self, peer: &PeerId) -> Option<u8> {
        self.routes.get(peer).map(|r| r.hops)
    }

    /// Start all queued transports and the packet pump.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        let mut pending = self.pending.lock().await;
        if pending.is_empty() && self.transports.read().await.is_empty() {
            return Err(AmaraError::NoTransport);
        }
        let mut transports = self.transports.write().await;
        for mut transport in pending.drain(..) {
            transport.start().await?;
            let index = transports.len();
            let rx = transport.events().await;
            let transport: Arc<dyn Transport> = Arc::from(transport);
            transports.push(transport.clone());
            let engine = self.clone();
            let handle = tokio::spawn(async move { engine.pump(index, transport, rx).await });
            self.tasks.lock().unwrap().push(handle);
        }
        let engine = self.clone();
        self.tasks
            .lock()
            .unwrap()
            .push(tokio::spawn(async move { engine.maintenance().await }));
        info!(
            "mesh started: {} ({})",
            self.identity.display_name(),
            self.identity.peer_id()
        );
        Ok(())
    }

    /// Stop background tasks (transports stop when dropped).
    pub fn shutdown(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    async fn pump(
        self: Arc<Self>,
        index: usize,
        transport: Arc<dyn Transport>,
        mut rx: broadcast::Receiver<TransportEvent>,
    ) {
        let mut assembler = FrameAssembler::default();
        loop {
            match rx.recv().await {
                Ok(TransportEvent::Received { from, data }) => match assembler.push(&data) {
                    Ok(Some(bytes)) => {
                        if let Err(e) = self.handle_raw_packet(&bytes, index, from).await {
                            trace!("dropped packet from link {from:?}: {e}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => trace!("bad frame from {from:?}: {e}"),
                },
                Ok(TransportEvent::PeerDiscovered { peer }) => {
                    debug!("link neighbor discovered on {}: {peer:?}", transport.name());
                    self.link_up.notify_one();
                }
                Ok(TransportEvent::PeerLost { peer }) => {
                    self.routes
                        .retain(|_, r| !(r.via == peer && r.transport == index));
                }
                Ok(TransportEvent::Error { message }) => {
                    warn!("{} transport error: {message}", transport.name())
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("{} lagged by {n} events", transport.name())
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    async fn maintenance(self: Arc<Self>) {
        let expiry = chrono::Duration::seconds(PEER_EXPIRY_SECS as i64);
        let route_ttl = Duration::from_secs(PEER_EXPIRY_SECS);
        loop {
            tokio::time::sleep(Duration::from_secs(ANNOUNCE_INTERVAL_SECS)).await;
            for peer in self.registry.all_peers() {
                if peer.is_expired(expiry) {
                    self.registry.remove_peer(&peer.peer_id);
                    self.routes.remove(&peer.peer_id);
                    let _ = self.event_tx.send(AmaraEvent::PeerLeft {
                        peer_id: peer.peer_id.to_string(),
                        name: peer.display_name,
                    });
                }
            }
            self.routes.retain(|_, r| r.updated.elapsed() < route_ttl);
            self.rate
                .retain(|_, (start, _)| start.elapsed() < Duration::from_secs(5));
        }
    }

    /// Sign and send a packet originated by this node.
    pub async fn send_packet(&self, mut packet: Packet) -> Result<()> {
        packet.header.ttl = packet.header.ttl.min(self.config.max_ttl);
        packet.sign(self.identity.signing_key());
        let bytes = packet.encode();
        let _ = self
            .dedup
            .is_duplicate(packet.header.source_id, packet.header.message_id);
        let dest = packet.header.destination_id;
        if dest.is_broadcast() {
            self.flood(&bytes, None).await;
            return Ok(());
        }
        self.forward(&bytes, dest, None).await
    }

    /// Send to the next hop for `dest`, or flood if no route is known.
    async fn forward(&self, bytes: &[u8], dest: PeerId, from: Option<PeerId>) -> Result<()> {
        let route = self.routes.get(&dest).map(|r| *r);
        if let Some(route) = route.filter(|r| Some(r.via) != from) {
            let transports = self.transports.read().await;
            if let Some(transport) = transports.get(route.transport) {
                match send_frames(transport.as_ref(), Some(&route.via), bytes).await {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        debug!(
                            "route to {dest:?} via {:?} failed ({e}); flooding",
                            route.via
                        );
                        self.routes.remove(&dest);
                    }
                }
            }
        }
        self.flood(bytes, from).await;
        Ok(())
    }

    async fn flood(&self, bytes: &[u8], _from: Option<PeerId>) {
        for transport in self.transports.read().await.iter() {
            if let Err(e) = send_frames(transport.as_ref(), None, bytes).await {
                debug!("broadcast on {} failed: {e}", transport.name());
            }
        }
    }

    fn within_rate(&self, source: PeerId) -> bool {
        let mut entry = self.rate.entry(source).or_insert((Instant::now(), 0));
        if entry.0.elapsed() >= Duration::from_secs(1) {
            *entry = (Instant::now(), 0);
        }
        entry.1 += 1;
        entry.1 <= RATE_LIMIT_PACKETS_PER_SEC
    }

    /// Process one complete packet received on transport `index` from link neighbor `from`.
    pub async fn handle_raw_packet(
        self: &Arc<Self>,
        data: &[u8],
        index: usize,
        from: PeerId,
    ) -> Result<()> {
        let packet = Packet::decode(data)?;
        let source = packet.header.source_id;
        if source == self.identity.peer_id() {
            return Ok(());
        }
        if self.config.blocked.contains(&source) || self.config.blocked.contains(&from) {
            return Err(AmaraError::PeerBlocked(source.to_string()));
        }
        if self.dedup.is_duplicate(source, packet.header.message_id) {
            return Ok(());
        }
        if !self.within_rate(source) {
            return Err(AmaraError::RateLimited(source.to_string()));
        }

        let sender_name = if packet.header.packet_type == PacketType::Announce {
            self.accept_announce(&packet, index, from)?
        } else {
            let peer = self
                .registry
                .get_peer(&source)
                .ok_or_else(|| AmaraError::UnknownPeer(source.to_string()))?;
            let key = ed25519_dalek::VerifyingKey::from_bytes(&peer.public_key).map_err(|_| {
                AmaraError::InvalidSignature {
                    peer: source.to_string(),
                }
            })?;
            if !packet.verify(&key) {
                self.security_event("invalid signature", source);
                return Err(AmaraError::InvalidSignature {
                    peer: source.to_string(),
                });
            }
            let skew = now_timestamp().abs_diff(packet.header.timestamp);
            if skew > MAX_CLOCK_SKEW_SECS {
                return Err(AmaraError::Expired);
            }
            self.registry.touch_peer(&source);
            peer.display_name
        };

        let dest = packet.header.destination_id;
        let for_us = dest == self.identity.peer_id() || dest.is_broadcast();
        let should_relay =
            self.config.relay_enabled && dest != self.identity.peer_id() && packet.should_relay();
        let relay = should_relay
            .then(|| packet.clone())
            .and_then(|mut r| r.header.decrement_ttl().then_some(r));

        if for_us {
            let _ = self.packet_tx.send(Inbound {
                packet,
                sender_name,
            });
        }

        if let Some(relay) = relay {
            let bytes = relay.encode();
            if dest.is_broadcast() {
                // Jitter de-synchronises neighbors that all heard the same flood.
                let engine = self.clone();
                tokio::spawn(async move {
                    let jitter =
                        rand::thread_rng().gen_range(RELAY_JITTER_MIN_MS..=RELAY_JITTER_MAX_MS);
                    tokio::time::sleep(Duration::from_millis(jitter)).await;
                    engine.flood(&bytes, Some(from)).await;
                });
            } else {
                self.forward(&bytes, dest, Some(from)).await?;
            }
        }
        Ok(())
    }

    /// Verify a self-certifying announce, learn the peer and its route.
    fn accept_announce(&self, packet: &Packet, index: usize, from: PeerId) -> Result<String> {
        let source = packet.header.source_id;
        let announce = Announce::decode(&packet.payload)?;
        if announce.peer_id() != source {
            self.security_event("announce key does not match PeerId", source);
            return Err(AmaraError::InvalidSignature {
                peer: source.to_string(),
            });
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&announce.public_key).map_err(|_| {
            AmaraError::InvalidSignature {
                peer: source.to_string(),
            }
        })?;
        if !packet.verify(&key) {
            self.security_event("invalid announce signature", source);
            return Err(AmaraError::InvalidSignature {
                peer: source.to_string(),
            });
        }

        let hops = DEFAULT_TTL
            .saturating_sub(packet.header.ttl)
            .saturating_add(1);
        let better = self.routes.get(&source).is_none_or(|r| {
            hops <= r.hops || r.updated.elapsed() > Duration::from_secs(ANNOUNCE_INTERVAL_SECS * 3)
        });
        if better {
            self.routes.insert(
                source,
                Route {
                    via: from,
                    transport: index,
                    hops,
                    updated: Instant::now(),
                },
            );
        }

        let existing = self.registry.get_peer(&source);
        let mut info = PeerInfo::new(
            announce.name.clone(),
            announce.public_key,
            announce.x25519_public,
        );
        info.capabilities = announce.capabilities;
        info.version = announce.version;
        info.platform = announce.platform;
        info.load_class = announce.load;
        info.availability = announce.availability;
        info.hop_count = self.routes.get(&source).map(|r| r.hops).unwrap_or(hops);
        info.next_hop = (from != source).then_some(from);
        info.trust_level = if self.config.trusted.contains(&source) {
            TrustLevel::Trusted
        } else {
            TrustLevel::Seen
        };
        info.transport = format!("link{index}");
        let capabilities_changed = existing
            .as_ref()
            .is_some_and(|e| e.capabilities != info.capabilities);
        self.registry.update_peer(source, info);

        if existing.is_none() {
            info!("peer joined: {} ({source}) {hops} hop(s)", announce.name);
            let _ = self.event_tx.send(AmaraEvent::PeerJoined {
                peer_id: source.to_string(),
                name: announce.name.clone(),
            });
        } else if capabilities_changed {
            let _ = self.event_tx.send(AmaraEvent::CapabilityChanged {
                peer_id: source.to_string(),
            });
        }
        Ok(announce.name)
    }

    fn security_event(&self, what: &str, peer: PeerId) {
        warn!("security: {what} ({peer})");
        let _ = self.event_tx.send(AmaraEvent::SecurityEvent {
            event: what.to_string(),
            peer_id: Some(peer.to_string()),
        });
    }
}

impl Drop for MeshEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn send_frames(transport: &dyn Transport, to: Option<&PeerId>, bytes: &[u8]) -> Result<()> {
    for frame in encode_frames(bytes, transport.mtu())? {
        match to {
            Some(peer) => transport.send(peer, &frame).await?,
            None => transport.broadcast(&frame).await?,
        }
    }
    Ok(())
}
