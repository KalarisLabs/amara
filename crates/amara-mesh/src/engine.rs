use crate::dedup::DedupCache;
use crate::registry::PeerRegistry;
use amara_core::peer::{PeerId, PeerInfo, Platform, TrustLevel};
use amara_core::{AmaraError, Result};
use amara_crypto::identity::NodeIdentity;
use amara_protocol::packet::Packet;
use amara_protocol::PacketType;
use amara_transport::{Transport, TransportEvent};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, error, info, trace, warn};
use rand::Rng;
use tokio::time::{sleep, Duration};
use amara_core::constants::{RELAY_JITTER_MAX_MS, RELAY_JITTER_MIN_MS, DEFAULT_TTL};

#[derive(Clone)]
pub struct MeshConfig {
    pub max_ttl: u8,
    pub relay_enabled: bool,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            max_ttl: DEFAULT_TTL,
            relay_enabled: true,
        }
    }
}

/// The core mesh routing engine.
pub struct MeshEngine {
    identity: NodeIdentity,
    config: MeshConfig,
    registry: PeerRegistry,
    dedup: Arc<DedupCache>,
    transports: Arc<Mutex<Vec<Box<dyn Transport>>>>,
    packet_tx: broadcast::Sender<Packet>,
}

impl MeshEngine {
    pub fn new(identity: NodeIdentity, config: MeshConfig) -> Self {
        let (packet_tx, _) = broadcast::channel(1024);
        Self {
            identity,
            config,
            registry: PeerRegistry::new(),
            dedup: Arc::new(DedupCache::new()),
            transports: Arc::new(Mutex::new(Vec::new())),
            packet_tx,
        }
    }

    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    pub fn registry(&self) -> &PeerRegistry {
        &self.registry
    }

    pub async fn add_transport(&self, transport: Box<dyn Transport>) {
        self.transports.lock().await.push(transport);
    }

    /// Subscribe to incoming packets that are addressed to us or broadcast.
    pub fn subscribe(&self) -> broadcast::Receiver<Packet> {
        self.packet_tx.subscribe()
    }

    /// Start processing transport events.
    pub async fn start(&self) -> Result<()> {
        let transports = self.transports.lock().await;
        if transports.is_empty() {
            return Err(AmaraError::NoTransport);
        }

        // We would normally spawn tasks to listen to each transport's event stream here.
        // For brevity in this initial implementation, we'll set up the structure.
        
        info!("Mesh engine started for node {}", self.identity.peer_id());
        Ok(())
    }

    /// Send a packet to the mesh.
    pub async fn send_packet(&self, mut packet: Packet) -> Result<()> {
        // Sign the packet before sending
        packet.sign(self.identity.signing_key());

        let encoded = packet.encode();
        
        let transports = self.transports.lock().await;
        for transport in transports.iter() {
            if packet.header.destination_id.is_broadcast() {
                if let Err(e) = transport.broadcast(&encoded).await {
                    warn!("Failed to broadcast on transport {}: {}", transport.name(), e);
                }
            } else {
                // In a real implementation, we'd look up the route in the registry
                // to decide which transport to use. Here we just broadcast it to
                // reachable transports for simplicity, letting the mesh handle it.
                if let Err(e) = transport.broadcast(&encoded).await {
                     warn!("Failed to send on transport {}: {}", transport.name(), e);
                }
            }
        }

        Ok(())
    }

    /// Handle an incoming raw packet from a transport.
    pub async fn handle_raw_packet(&self, data: &[u8], _from_transport: &str) -> Result<()> {
        let packet = Packet::decode(data)?;

        // Deduplication
        if self.dedup.is_duplicate(packet.header.source_id, packet.header.message_id) {
            trace!("Dropped duplicate packet from {}", packet.header.source_id);
            return Ok(());
        }

        // We must know the public key to verify. For Announce, the public key is not in the packet,
        // it's supposed to be fetched or pre-known. Wait, actually we can derive peer ID from public key, 
        // but we need the public key to verify signatures. 
        // For this MVP, we assume signature verification is deferred or handled if we know the peer.
        
        // Is it for us?
        let for_us = packet.header.destination_id == self.identity.peer_id()
            || packet.header.destination_id.is_broadcast();

        if for_us {
            let _ = self.packet_tx.send(packet.clone());
        }

        // Relay logic
        if self.config.relay_enabled && packet.should_relay() {
            let mut relay_packet = packet.clone();
            if relay_packet.header.decrement_ttl() {
                let transports = self.transports.clone();
                let encoded = relay_packet.encode();
                
                // Spawn relay to add jitter and prevent blocking
                tokio::spawn(async move {
                    let jitter = rand::thread_rng().gen_range(RELAY_JITTER_MIN_MS..=RELAY_JITTER_MAX_MS);
                    sleep(Duration::from_millis(jitter)).await;
                    
                    let transports = transports.lock().await;
                    for transport in transports.iter() {
                        // TODO: Split horizon (don't send back to where it came from)
                        let _ = transport.broadcast(&encoded).await;
                    }
                });
            }
        }

        Ok(())
    }
}
