use crate::traits::{Transport, TransportEvent};
use amara_core::peer::PeerId;
use amara_core::{AmaraError, Result};
use async_trait::async_trait;
use tokio::sync::broadcast;

/// A simple loopback transport for local testing.
/// Broadcasts are sent to self.
pub struct LoopbackTransport {
    local_id: PeerId,
    tx: broadcast::Sender<TransportEvent>,
    is_running: bool,
}

impl LoopbackTransport {
    pub fn new(local_id: PeerId) -> Self {
        let (tx, _) = broadcast::channel(100);
        Self {
            local_id,
            tx,
            is_running: false,
        }
    }
}

#[async_trait]
impl Transport for LoopbackTransport {
    async fn start(&mut self) -> Result<()> {
        self.is_running = true;
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

        if destination == &self.local_id || destination.is_broadcast() {
            let _ = self.tx.send(TransportEvent::Received {
                from: self.local_id,
                data: data.to_vec(),
            });
        }
        Ok(())
    }

    async fn broadcast(&self, data: &[u8]) -> Result<()> {
        if !self.is_running {
            return Err(AmaraError::Transport("Transport not running".into()));
        }

        let _ = self.tx.send(TransportEvent::Received {
            from: self.local_id,
            data: data.to_vec(),
        });
        Ok(())
    }

    async fn events(&self) -> broadcast::Receiver<TransportEvent> {
        self.tx.subscribe()
    }

    fn name(&self) -> &str {
        "loopback"
    }
}
