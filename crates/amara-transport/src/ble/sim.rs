//! In-memory Bluetooth "air" for tests and simulations: radios connect in
//! pairs, writes are limited to the BLE write size, and links can drop.

use super::{BleRadio, DEFAULT_WRITE_SIZE, LinkId, RadioEvent};
use amara_core::peer::PeerId;
use amara_core::{AmaraError, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Default)]
struct AirState {
    radios: HashMap<PeerId, mpsc::Sender<RadioEvent>>,
    links: HashSet<(PeerId, PeerId)>,
}

/// Shared medium for [`SimRadio`]s.
#[derive(Clone, Default)]
pub struct BleAir {
    state: Arc<Mutex<AirState>>,
}

/// The link id a radio uses for its connection to `peer`.
pub fn link_to(peer: PeerId) -> LinkId {
    u64::from_be_bytes(*peer.as_bytes())
}

impl BleAir {
    pub fn new() -> Self {
        Self::default()
    }

    /// A radio for the node `id`, attached to this air.
    pub fn radio(&self, id: PeerId) -> SimRadio {
        SimRadio {
            id,
            air: self.clone(),
        }
    }

    fn sender(&self, id: &PeerId) -> Option<mpsc::Sender<RadioEvent>> {
        self.state.lock().unwrap().radios.get(id).cloned()
    }

    /// Bring two radios into range (both must be started).
    pub async fn connect(&self, a: PeerId, b: PeerId) {
        {
            let mut s = self.state.lock().unwrap();
            s.links.insert((a, b));
            s.links.insert((b, a));
        }
        for (me, other) in [(a, b), (b, a)] {
            if let Some(tx) = self.sender(&me) {
                let _ = tx.send(RadioEvent::Up(link_to(other))).await;
            }
        }
    }

    /// Take two radios out of range.
    pub async fn disconnect(&self, a: PeerId, b: PeerId) {
        {
            let mut s = self.state.lock().unwrap();
            s.links.remove(&(a, b));
            s.links.remove(&(b, a));
        }
        for (me, other) in [(a, b), (b, a)] {
            if let Some(tx) = self.sender(&me) {
                let _ = tx.send(RadioEvent::Down(link_to(other))).await;
            }
        }
    }

    fn neighbors(&self, id: &PeerId) -> Vec<PeerId> {
        let s = self.state.lock().unwrap();
        s.links
            .iter()
            .filter(|(from, _)| from == id)
            .map(|(_, to)| *to)
            .collect()
    }
}

/// One node's radio on a [`BleAir`].
pub struct SimRadio {
    id: PeerId,
    air: BleAir,
}

impl SimRadio {
    async fn deliver(&self, to: PeerId, data: &[u8]) -> Result<()> {
        if data.len() > DEFAULT_WRITE_SIZE {
            return Err(AmaraError::Ble(format!(
                "write of {} bytes exceeds {DEFAULT_WRITE_SIZE}",
                data.len()
            )));
        }
        let tx = self
            .air
            .sender(&to)
            .ok_or_else(|| AmaraError::Ble("radio off".into()))?;
        tx.send(RadioEvent::Data(link_to(self.id), data.to_vec()))
            .await
            .map_err(|_| AmaraError::Ble("radio off".into()))
    }
}

#[async_trait]
impl BleRadio for SimRadio {
    async fn start(&self, events: mpsc::Sender<RadioEvent>) -> Result<()> {
        self.air
            .state
            .lock()
            .unwrap()
            .radios
            .insert(self.id, events);
        Ok(())
    }

    async fn send(&self, link: LinkId, data: &[u8]) -> Result<()> {
        let to = self
            .air
            .neighbors(&self.id)
            .into_iter()
            .find(|p| link_to(*p) == link)
            .ok_or_else(|| AmaraError::Ble("link down".into()))?;
        self.deliver(to, data).await
    }

    async fn broadcast(&self, data: &[u8]) -> Result<()> {
        for to in self.air.neighbors(&self.id) {
            let _ = self.deliver(to, data).await;
        }
        Ok(())
    }

    async fn stop(&self) {
        self.air.state.lock().unwrap().radios.remove(&self.id);
    }

    fn name(&self) -> &str {
        "ble-sim"
    }
}
