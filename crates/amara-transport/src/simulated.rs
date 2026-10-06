use crate::traits::{Transport, TransportEvent};
use amara_core::peer::PeerId;
use amara_core::{AmaraError, Result};
use async_trait::async_trait;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::time::{sleep, Duration};

/// Configuration for a link between two simulated nodes.
#[derive(Debug, Clone, Copy)]
pub struct LinkConfig {
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub drop_probability: f32, // 0.0 to 1.0
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            latency_min_ms: 10,
            latency_max_ms: 50,
            drop_probability: 0.0,
        }
    }
}

/// A simulated network hub connecting multiple SimulatedTransports.
#[derive(Clone)]
pub struct SimulatorHub {
    nodes: Arc<Mutex<HashMap<PeerId, mpsc::Sender<TransportEvent>>>>,
    topology: Arc<Mutex<HashMap<(PeerId, PeerId), LinkConfig>>>,
}

impl SimulatorHub {
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(Mutex::new(HashMap::new())),
            topology: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new node with the hub.
    pub async fn register(&self, peer_id: PeerId) -> mpsc::Receiver<TransportEvent> {
        let (tx, rx) = mpsc::channel(1024);
        self.nodes.lock().await.insert(peer_id, tx);
        rx
    }

    /// Connect two nodes bidirectionally.
    pub async fn connect(&self, a: PeerId, b: PeerId, config: LinkConfig) {
        let mut top = self.topology.lock().await;
        top.insert((a, b), config);
        top.insert((b, a), config);
        
        // Notify both nodes they discovered each other
        let nodes = self.nodes.lock().await;
        if let Some(tx) = nodes.get(&a) {
            let _ = tx.send(TransportEvent::PeerDiscovered { peer: b }).await;
        }
        if let Some(tx) = nodes.get(&b) {
            let _ = tx.send(TransportEvent::PeerDiscovered { peer: a }).await;
        }
    }

    /// Disconnect two nodes.
    pub async fn disconnect(&self, a: PeerId, b: PeerId) {
        let mut top = self.topology.lock().await;
        top.remove(&(a, b));
        top.remove(&(b, a));

        // Notify
        let nodes = self.nodes.lock().await;
        if let Some(tx) = nodes.get(&a) {
            let _ = tx.send(TransportEvent::PeerLost { peer: b }).await;
        }
        if let Some(tx) = nodes.get(&b) {
            let _ = tx.send(TransportEvent::PeerLost { peer: a }).await;
        }
    }

    /// Broadcast data from a source node to all connected nodes.
    pub async fn broadcast(&self, from: PeerId, data: Vec<u8>) {
        let topology = self.topology.lock().await.clone();
        let nodes = self.nodes.lock().await.clone();

        for ((src, dst), config) in topology {
            if src == from {
                if let Some(tx) = nodes.get(&dst) {
                    Self::deliver(tx.clone(), from, data.clone(), config).await;
                }
            }
        }
    }

    /// Send data directly to a specific connected node.
    pub async fn send(&self, from: PeerId, to: PeerId, data: Vec<u8>) -> Result<()> {
        let config = self.topology.lock().await.get(&(from, to)).copied();
        
        if let Some(config) = config {
            let tx = self.nodes.lock().await.get(&to).cloned();
            if let Some(tx) = tx {
                Self::deliver(tx, from, data, config).await;
                return Ok(());
            }
        }
        Err(AmaraError::Transport("Peer not reachable".into()))
    }

    async fn deliver(
        tx: mpsc::Sender<TransportEvent>,
        from: PeerId,
        data: Vec<u8>,
        config: LinkConfig,
    ) {
        tokio::spawn(async move {
            let mut rng = rand::thread_rng();
            if config.drop_probability > 0.0 && rng.gen::<f32>() < config.drop_probability {
                return; // Packet dropped
            }

            let latency = rng.gen_range(config.latency_min_ms..=config.latency_max_ms);
            sleep(Duration::from_millis(latency)).await;

            let _ = tx.send(TransportEvent::Received { from, data }).await;
        });
    }
}

/// A node in the simulated network implementing the Transport trait.
pub struct SimulatedTransport {
    local_id: PeerId,
    hub: SimulatorHub,
    rx: Option<mpsc::Receiver<TransportEvent>>,
    events_tx: broadcast::Sender<TransportEvent>,
    is_running: bool,
}

impl SimulatedTransport {
    pub async fn new(local_id: PeerId, hub: SimulatorHub) -> Self {
        let rx = hub.register(local_id).await;
        let (events_tx, _) = broadcast::channel(1024);
        
        Self {
            local_id,
            hub,
            rx: Some(rx),
            events_tx,
            is_running: false,
        }
    }
}

#[async_trait]
impl Transport for SimulatedTransport {
    async fn start(&mut self) -> Result<()> {
        if self.is_running {
            return Ok(());
        }
        self.is_running = true;

        if let Some(mut rx) = self.rx.take() {
            let tx = self.events_tx.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    let _ = tx.send(event);
                }
            });
        }
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.is_running = false;
        Ok(())
    }

    async fn send(&self, destination: &PeerId, data: &[u8]) -> Result<()> {
        if !self.is_running {
            return Err(AmaraError::Transport("Transport not running".into()));
        }
        self.hub.send(self.local_id, *destination, data.to_vec()).await
    }

    async fn broadcast(&self, data: &[u8]) -> Result<()> {
        if !self.is_running {
            return Err(AmaraError::Transport("Transport not running".into()));
        }
        self.hub.broadcast(self.local_id, data.to_vec()).await;
        Ok(())
    }

    async fn events(&self) -> broadcast::Receiver<TransportEvent> {
        self.events_tx.subscribe()
    }

    fn name(&self) -> &str {
        "simulated"
    }
}

/// Convenience struct to represent a simulated node and its transport.
pub struct SimulatorNode {
    pub peer_id: PeerId,
    pub transport: SimulatedTransport,
}

impl SimulatorNode {
    pub async fn new(peer_id: PeerId, hub: SimulatorHub) -> Self {
        Self {
            peer_id,
            transport: SimulatedTransport::new(peer_id, hub).await,
        }
    }
}
