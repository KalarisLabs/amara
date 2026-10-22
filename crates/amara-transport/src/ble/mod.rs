//! Bluetooth LE transport: phones, Raspberry Pis and laptops meeting with no
//! network at all.
//!
//! ## GATT layout
//!
//! | Item | UUID | Properties |
//! |---|---|---|
//! | AMARA service | [`SERVICE_UUID`] | advertised |
//! | RX (central → peripheral) | [`RX_CHAR_UUID`] | write, write without response |
//! | TX (peripheral → centrals) | [`TX_CHAR_UUID`] | notify |
//!
//! Every node is both roles at once (like BitChat): it advertises the service
//! and also scans for and connects to others. Each GATT write or notification
//! carries one link datagram:
//!
//! ```text
//! "AM" 0x02 · link_sender (8) · link_dest (8, zeros = broadcast) · frame
//! ```
//!
//! `link_dest` exists because a peripheral's notification reaches every
//! subscribed central, so unicast over the peripheral role is filtered by the
//! receivers. Frames are the mesh's per-hop frames; the transport's MTU is the
//! write size minus this header, and the mesh fragments anything larger.
//!
//! The radio itself sits behind [`BleRadio`], so the same link logic runs on
//! the native stack ([`NativeRadio`], feature `ble`), on the in-memory
//! [`BleAir`] used by tests and simulations, or on a phone's own Bluetooth API.

#[cfg(feature = "ble")]
mod native;
mod sim;

#[cfg(feature = "ble")]
pub use native::{NativeRadio, NativeRadioConfig};
pub use sim::{BleAir, SimRadio};

use crate::traits::{Transport, TransportEvent};
use amara_core::peer::PeerId;
use amara_core::{AmaraError, PEER_EXPIRY_SECS, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::debug;

/// AMARA GATT service (`AMARA` in the first bytes).
pub const SERVICE_UUID: u128 = 0x414d_4152_4100_4d65_7368_0000_0000_0001;
/// Characteristic centrals write datagrams to.
pub const RX_CHAR_UUID: u128 = 0x414d_4152_4100_4d65_7368_0000_0000_0002;
/// Characteristic peripherals notify datagrams on.
pub const TX_CHAR_UUID: u128 = 0x414d_4152_4100_4d65_7368_0000_0000_0003;

const PREFIX: [u8; 3] = [b'A', b'M', 0x02];
/// Bytes of link header in every BLE datagram.
pub const DATAGRAM_OVERHEAD: usize = PREFIX.len() + 8 + 8;
/// Default GATT payload per write/notification. Every current phone and
/// desktop stack negotiates an ATT MTU of at least 185 (182 usable).
pub const DEFAULT_WRITE_SIZE: usize = 182;

/// Opaque handle for one GATT connection (or, for the peripheral role, the
/// set of subscribed centrals).
pub type LinkId = u64;

/// What a radio reports to the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioEvent {
    Up(LinkId),
    Down(LinkId),
    Data(LinkId, Vec<u8>),
}

/// A BLE stack: something that can move small datagrams over links.
#[async_trait]
pub trait BleRadio: Send + Sync + 'static {
    /// Begin advertising/scanning; report link changes and data on `events`.
    async fn start(&self, events: mpsc::Sender<RadioEvent>) -> Result<()>;
    /// Send one datagram on one link.
    async fn send(&self, link: LinkId, data: &[u8]) -> Result<()>;
    /// Send one datagram on every link.
    async fn broadcast(&self, data: &[u8]) -> Result<()>;
    async fn stop(&self) {}
    /// Largest datagram one write/notification can carry.
    fn max_write(&self) -> usize {
        DEFAULT_WRITE_SIZE
    }
    fn name(&self) -> &str {
        "ble"
    }
}

#[derive(Debug, Clone, Copy)]
struct Neighbor {
    link: LinkId,
    last_seen: Instant,
}

type Neighbors = Arc<Mutex<HashMap<PeerId, Neighbor>>>;

/// Mesh transport over any [`BleRadio`].
pub struct BleTransport<R: BleRadio> {
    local_id: PeerId,
    radio: Arc<R>,
    neighbors: Neighbors,
    events_tx: broadcast::Sender<TransportEvent>,
    tasks: Vec<JoinHandle<()>>,
    running: bool,
}

impl<R: BleRadio> BleTransport<R> {
    pub fn new(local_id: PeerId, radio: R) -> Self {
        Self {
            local_id,
            radio: Arc::new(radio),
            neighbors: Arc::new(Mutex::new(HashMap::new())),
            events_tx: broadcast::channel(4096).0,
            tasks: Vec::new(),
            running: false,
        }
    }

    pub fn radio(&self) -> &Arc<R> {
        &self.radio
    }

    fn datagram(&self, dest: &PeerId, frame: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(DATAGRAM_OVERHEAD + frame.len());
        buf.extend_from_slice(&PREFIX);
        buf.extend_from_slice(self.local_id.as_bytes());
        buf.extend_from_slice(dest.as_bytes());
        buf.extend_from_slice(frame);
        buf
    }
}

/// Parse a datagram: `(sender, dest, frame)`.
fn parse(datagram: &[u8]) -> Option<(PeerId, PeerId, &[u8])> {
    if datagram.len() < DATAGRAM_OVERHEAD || datagram[..3] != PREFIX {
        return None;
    }
    let mut sender = [0u8; 8];
    sender.copy_from_slice(&datagram[3..11]);
    let mut dest = [0u8; 8];
    dest.copy_from_slice(&datagram[11..19]);
    Some((
        PeerId::from_bytes(sender),
        PeerId::from_bytes(dest),
        &datagram[DATAGRAM_OVERHEAD..],
    ))
}

#[async_trait]
impl<R: BleRadio> Transport for BleTransport<R> {
    async fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        let (tx, mut rx) = mpsc::channel(1024);
        self.radio.start(tx).await?;
        self.running = true;

        let local_id = self.local_id;
        let neighbors = self.neighbors.clone();
        let events = self.events_tx.clone();
        self.tasks.push(tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    RadioEvent::Up(link) => debug!("ble link {link} up"),
                    RadioEvent::Down(link) => {
                        let lost: Vec<PeerId> = {
                            let mut map = neighbors.lock().unwrap();
                            let lost: Vec<PeerId> = map
                                .iter()
                                .filter(|(_, n)| n.link == link)
                                .map(|(id, _)| *id)
                                .collect();
                            for id in &lost {
                                map.remove(id);
                            }
                            lost
                        };
                        for peer in lost {
                            let _ = events.send(TransportEvent::PeerLost { peer });
                        }
                    }
                    RadioEvent::Data(link, data) => {
                        let Some((from, dest, frame)) = parse(&data) else {
                            continue;
                        };
                        if from == local_id || !(dest == local_id || dest.is_broadcast()) {
                            continue;
                        }
                        let is_new = neighbors
                            .lock()
                            .unwrap()
                            .insert(
                                from,
                                Neighbor {
                                    link,
                                    last_seen: Instant::now(),
                                },
                            )
                            .is_none_or(|n| n.link != link);
                        if is_new {
                            let _ = events.send(TransportEvent::PeerDiscovered { peer: from });
                        }
                        let _ = events.send(TransportEvent::Received {
                            from,
                            data: frame.to_vec(),
                        });
                    }
                }
            }
        }));

        let neighbors = self.neighbors.clone();
        let events = self.events_tx.clone();
        self.tasks.push(tokio::spawn(async move {
            let ttl = Duration::from_secs(PEER_EXPIRY_SECS);
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let lost: Vec<PeerId> = {
                    let mut map = neighbors.lock().unwrap();
                    let lost: Vec<PeerId> = map
                        .iter()
                        .filter(|(_, n)| n.last_seen.elapsed() > ttl)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in &lost {
                        map.remove(id);
                    }
                    lost
                };
                for peer in lost {
                    let _ = events.send(TransportEvent::PeerLost { peer });
                }
            }
        }));
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.radio.stop().await;
        self.running = false;
        Ok(())
    }

    async fn send(&self, destination: &PeerId, data: &[u8]) -> Result<()> {
        if !self.running {
            return Err(AmaraError::Ble("ble transport not running".into()));
        }
        let link = self
            .neighbors
            .lock()
            .unwrap()
            .get(destination)
            .map(|n| n.link)
            .ok_or_else(|| AmaraError::UnknownPeer(destination.to_string()))?;
        self.radio
            .send(link, &self.datagram(destination, data))
            .await
    }

    async fn broadcast(&self, data: &[u8]) -> Result<()> {
        if !self.running {
            return Err(AmaraError::Ble("ble transport not running".into()));
        }
        self.radio
            .broadcast(&self.datagram(&PeerId::broadcast(), data))
            .await
    }

    async fn events(&self) -> broadcast::Receiver<TransportEvent> {
        self.events_tx.subscribe()
    }

    fn name(&self) -> &str {
        self.radio.name()
    }

    fn mtu(&self) -> usize {
        self.radio.max_write().saturating_sub(DATAGRAM_OVERHEAD)
    }
}

impl<R: BleRadio> Drop for BleTransport<R> {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn recv_data(rx: &mut broadcast::Receiver<TransportEvent>) -> (PeerId, Vec<u8>) {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(TransportEvent::Received { from, data })) => return (from, data),
                Ok(Ok(_)) => continue,
                other => panic!("no data received: {other:?}"),
            }
        }
    }

    #[test]
    fn datagram_parsing() {
        assert!(parse(b"AM\x02short").is_none());
        let mut d = PREFIX.to_vec();
        d.extend_from_slice(&[1; 8]);
        d.extend_from_slice(&[0; 8]);
        d.extend_from_slice(b"frame");
        let (from, dest, frame) = parse(&d).unwrap();
        assert_eq!(from, PeerId::from_bytes([1; 8]));
        assert!(dest.is_broadcast());
        assert_eq!(frame, b"frame");
    }

    #[tokio::test]
    async fn unicast_is_filtered_by_link_destination() {
        let air = BleAir::new();
        let (a, b, c) = (
            PeerId::from_bytes([1; 8]),
            PeerId::from_bytes([2; 8]),
            PeerId::from_bytes([3; 8]),
        );
        let mut ta = BleTransport::new(a, air.radio(a));
        let mut tb = BleTransport::new(b, air.radio(b));
        let mut tc = BleTransport::new(c, air.radio(c));
        for t in [&mut ta, &mut tb, &mut tc] {
            t.start().await.unwrap();
        }
        air.connect(a, b).await;
        air.connect(a, c).await;
        let (mut rb, mut rc, mut ra) = (tb.events().await, tc.events().await, ta.events().await);

        tb.broadcast(b"hello").await.unwrap();
        assert_eq!(recv_data(&mut ra).await, (b, b"hello".to_vec()));
        ta.send(&b, b"for b only").await.unwrap();
        assert_eq!(recv_data(&mut rb).await, (a, b"for b only".to_vec()));
        // A datagram for b that reaches c (as a peripheral notification would) is dropped.
        ta.radio()
            .send(sim::link_to(c), &ta.datagram(&b, b"for b only"))
            .await
            .unwrap();
        ta.broadcast(b"all").await.unwrap();
        assert_eq!(
            recv_data(&mut rc).await,
            (a, b"all".to_vec()),
            "c never saw b's unicast"
        );
        assert!(ta.mtu() < DEFAULT_WRITE_SIZE);
        assert!(ta.send(&PeerId::from_bytes([9; 8]), b"x").await.is_err());
    }

    #[tokio::test]
    async fn link_down_loses_peers() {
        let air = BleAir::new();
        let (a, b) = (PeerId::from_bytes([1; 8]), PeerId::from_bytes([2; 8]));
        let mut ta = BleTransport::new(a, air.radio(a));
        let mut tb = BleTransport::new(b, air.radio(b));
        ta.start().await.unwrap();
        tb.start().await.unwrap();
        air.connect(a, b).await;
        let mut ra = ta.events().await;
        tb.broadcast(b"hi").await.unwrap();
        recv_data(&mut ra).await;
        air.disconnect(a, b).await;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ra.recv()).await {
                Ok(Ok(TransportEvent::PeerLost { peer })) => {
                    assert_eq!(peer, b);
                    break;
                }
                Ok(Ok(_)) => continue,
                other => panic!("expected PeerLost, got {other:?}"),
            }
        }
        assert!(ta.send(&b, b"x").await.is_err());
    }
}
