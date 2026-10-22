use amara_core::Result;
use amara_core::peer::PeerId;
use async_trait::async_trait;
use tokio::sync::broadcast;

/// Events emitted by a transport layer.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// Received data from a peer.
    Received { from: PeerId, data: Vec<u8> },
    /// A new peer was discovered by the transport.
    PeerDiscovered { peer: PeerId },
    /// A peer was lost by the transport.
    PeerLost { peer: PeerId },
    /// Transport encountered a fatal error.
    Error { message: String },
}

/// Abstract transport layer for the AMARA mesh.
///
/// A transport is responsible for sending and receiving raw bytes
/// between physical or virtual nodes. It knows nothing about AMARA
/// packet structures or mesh routing.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Start the transport.
    async fn start(&mut self) -> Result<()>;

    /// Stop the transport.
    async fn stop(&mut self) -> Result<()>;

    /// Send data directly to a link-level neighbor.
    async fn send(&self, destination: &PeerId, data: &[u8]) -> Result<()>;

    /// Broadcast data to all reachable peers.
    async fn broadcast(&self, data: &[u8]) -> Result<()>;

    /// Subscribe to events from this transport.
    async fn events(&self) -> broadcast::Receiver<TransportEvent>;

    /// Human-readable name of this transport (e.g., "ble", "simulated").
    fn name(&self) -> &str;

    /// Largest frame this transport can carry in one send. The mesh fragments
    /// packets above this size.
    fn mtu(&self) -> usize {
        usize::MAX
    }
}
