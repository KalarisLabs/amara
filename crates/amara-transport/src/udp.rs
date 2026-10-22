//! Zero-configuration LAN transport.
//!
//! * **Discovery / broadcast**: IPv4 multicast group `239.77.65.82:7433`, TTL 1,
//!   so frames never leave the local network segment (proximity by design).
//! * **Unicast**: each node also owns an ephemeral UDP socket. Neighbors learn
//!   that address from the source of any datagram and reply to it directly.
//! * **Static peers**: optional `host:port` list for networks without multicast
//!   (VPNs such as Tailscale/WireGuard, cloud VMs, Docker bridges).
//!
//! Datagram layout: `"AM" 0x01 sender_peer_id(8) frame`.

use crate::traits::{Transport, TransportEvent};
use amara_core::peer::PeerId;
use amara_core::{
    AmaraError, DEFAULT_MESH_PORT, MESH_MULTICAST_V4, PEER_EXPIRY_SECS, Result, UDP_MTU,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

const PREFIX: [u8; 3] = [b'A', b'M', 0x01];
const DATAGRAM_OVERHEAD: usize = PREFIX.len() + 8;

/// UDP transport settings.
#[derive(Debug, Clone)]
pub struct UdpConfig {
    /// Multicast port shared by every node on the LAN.
    pub port: u16,
    /// Multicast group.
    pub group: Ipv4Addr,
    /// Join the multicast group (disable for unicast-only deployments).
    pub multicast: bool,
    /// Extra peers reached by unicast (`host:port` of their multicast port).
    pub static_peers: Vec<SocketAddr>,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_MESH_PORT,
            group: Ipv4Addr::from(MESH_MULTICAST_V4),
            multicast: true,
            static_peers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Neighbor {
    addr: SocketAddr,
    last_seen: Instant,
    /// Heard via multicast, so broadcasts reach it without a unicast copy.
    via_multicast: bool,
}

type Neighbors = Arc<Mutex<HashMap<PeerId, Neighbor>>>;

/// LAN mesh transport over UDP.
pub struct UdpTransport {
    local_id: PeerId,
    config: UdpConfig,
    unicast: Option<Arc<UdpSocket>>,
    neighbors: Neighbors,
    events_tx: broadcast::Sender<TransportEvent>,
    tasks: Vec<JoinHandle<()>>,
}

impl UdpTransport {
    pub fn new(local_id: PeerId, config: UdpConfig) -> Self {
        let (events_tx, _) = broadcast::channel(4096);
        Self {
            local_id,
            config,
            unicast: None,
            neighbors: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
            tasks: Vec::new(),
        }
    }

    /// Local unicast address (after `start`).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.unicast.as_ref().and_then(|s| s.local_addr().ok())
    }

    fn multicast_socket(&self) -> std::io::Result<UdpSocket> {
        use socket2::{Domain, Protocol, Socket, Type};
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        socket.set_reuse_port(true)?;
        let bind = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, self.config.port);
        socket.bind(&SocketAddr::V4(bind).into())?;
        socket.join_multicast_v4(&self.config.group, &Ipv4Addr::UNSPECIFIED)?;
        socket.set_multicast_loop_v4(true)?;
        socket.set_nonblocking(true)?;
        UdpSocket::from_std(socket.into())
    }

    fn frame(&self, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(DATAGRAM_OVERHEAD + data.len());
        buf.extend_from_slice(&PREFIX);
        buf.extend_from_slice(self.local_id.as_bytes());
        buf.extend_from_slice(data);
        buf
    }

    fn spawn_reader(&self, socket: Arc<UdpSocket>, via_multicast: bool) -> JoinHandle<()> {
        let local_id = self.local_id;
        let neighbors = self.neighbors.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65_536];
            loop {
                let (len, addr) = match socket.recv_from(&mut buf).await {
                    Ok(v) => v,
                    Err(e) => {
                        // Windows reports ICMP port-unreachable as a recv error; keep going.
                        debug!("udp recv error: {e}");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                };
                let datagram = &buf[..len];
                if len < DATAGRAM_OVERHEAD || datagram[..3] != PREFIX {
                    continue;
                }
                let mut id = [0u8; 8];
                id.copy_from_slice(&datagram[3..11]);
                let from = PeerId::from_bytes(id);
                if from == local_id {
                    continue; // our own multicast echo
                }
                let is_new = {
                    let mut map = neighbors.lock().unwrap();
                    let is_new = map.get(&from).is_none_or(|n| n.addr != addr);
                    let entry = map.entry(from).or_insert(Neighbor {
                        addr,
                        last_seen: Instant::now(),
                        via_multicast,
                    });
                    // Multicast copies come from the sender's unicast socket too,
                    // so the source address is always the right reply target.
                    entry.addr = addr;
                    entry.last_seen = Instant::now();
                    entry.via_multicast |= via_multicast;
                    is_new
                };
                if is_new {
                    let _ = events.send(TransportEvent::PeerDiscovered { peer: from });
                }
                let _ = events.send(TransportEvent::Received {
                    from,
                    data: datagram[DATAGRAM_OVERHEAD..].to_vec(),
                });
            }
        })
    }

    fn spawn_reaper(&self) -> JoinHandle<()> {
        let neighbors = self.neighbors.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let ttl = Duration::from_secs(PEER_EXPIRY_SECS);
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let lost: Vec<PeerId> = {
                    let mut map = neighbors.lock().unwrap();
                    let lost = map
                        .iter()
                        .filter(|(_, n)| n.last_seen.elapsed() > ttl)
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>();
                    for id in &lost {
                        map.remove(id);
                    }
                    lost
                };
                for peer in lost {
                    let _ = events.send(TransportEvent::PeerLost { peer });
                }
            }
        })
    }
}

#[async_trait]
impl Transport for UdpTransport {
    async fn start(&mut self) -> Result<()> {
        if self.unicast.is_some() {
            return Ok(());
        }
        let unicast = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| AmaraError::Transport(format!("udp bind: {e}")))?;
        let _ = unicast.set_multicast_loop_v4(true);
        let _ = unicast.set_multicast_ttl_v4(1);
        let unicast = Arc::new(unicast);
        self.tasks.push(self.spawn_reader(unicast.clone(), false));

        if self.config.multicast {
            match self.multicast_socket() {
                Ok(socket) => self.tasks.push(self.spawn_reader(Arc::new(socket), true)),
                Err(e) => warn!(
                    "LAN multicast unavailable on port {} ({e}); using unicast/static peers only",
                    self.config.port
                ),
            }
        }
        self.tasks.push(self.spawn_reaper());
        self.unicast = Some(unicast);
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.unicast = None;
        Ok(())
    }

    async fn send(&self, destination: &PeerId, data: &[u8]) -> Result<()> {
        let socket = self
            .unicast
            .as_ref()
            .ok_or_else(|| AmaraError::Transport("udp transport not running".into()))?;
        let addr = self
            .neighbors
            .lock()
            .unwrap()
            .get(destination)
            .map(|n| n.addr)
            .ok_or_else(|| AmaraError::UnknownPeer(destination.to_string()))?;
        socket
            .send_to(&self.frame(data), addr)
            .await
            .map_err(|e| AmaraError::Transport(format!("udp send: {e}")))?;
        Ok(())
    }

    async fn broadcast(&self, data: &[u8]) -> Result<()> {
        let socket = self
            .unicast
            .as_ref()
            .ok_or_else(|| AmaraError::Transport("udp transport not running".into()))?;
        let frame = self.frame(data);
        if self.config.multicast {
            let group = SocketAddr::V4(SocketAddrV4::new(self.config.group, self.config.port));
            if let Err(e) = socket.send_to(&frame, group).await {
                debug!("multicast send failed: {e}");
            }
        }
        // Neighbors that are not reachable by multicast get a unicast copy.
        let mut targets: Vec<SocketAddr> = self
            .neighbors
            .lock()
            .unwrap()
            .values()
            .filter(|n| !n.via_multicast || !self.config.multicast)
            .map(|n| n.addr)
            .collect();
        for peer in &self.config.static_peers {
            if !targets.contains(peer) {
                targets.push(*peer);
            }
        }
        for addr in targets {
            if let Err(e) = socket.send_to(&frame, addr).await {
                debug!("unicast copy to {addr} failed: {e}");
            }
        }
        Ok(())
    }

    async fn events(&self) -> broadcast::Receiver<TransportEvent> {
        self.events_tx.subscribe()
    }

    fn name(&self) -> &str {
        "udp"
    }

    fn mtu(&self) -> usize {
        UDP_MTU - DATAGRAM_OVERHEAD
    }
}

impl Drop for UdpTransport {
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

    #[tokio::test]
    async fn unicast_via_static_peer() {
        // Unicast-only nodes on loopback, bootstrapped by a static peer address.
        let a_id = PeerId::from_bytes([1; 8]);
        let b_id = PeerId::from_bytes([2; 8]);
        let mut b = UdpTransport::new(
            b_id,
            UdpConfig {
                multicast: false,
                ..Default::default()
            },
        );
        b.start().await.unwrap();
        let b_port = b.local_addr().unwrap().port();
        let mut a = UdpTransport::new(
            a_id,
            UdpConfig {
                multicast: false,
                static_peers: vec![SocketAddr::from(([127, 0, 0, 1], b_port))],
                ..Default::default()
            },
        );
        a.start().await.unwrap();
        let mut b_rx = b.events().await;
        let mut a_rx = a.events().await;

        a.broadcast(b"hello").await.unwrap();
        assert_eq!(recv_data(&mut b_rx).await, (a_id, b"hello".to_vec()));

        // b learned a's address from the datagram and can now reply directly.
        b.send(&a_id, b"hi back").await.unwrap();
        assert_eq!(recv_data(&mut a_rx).await, (b_id, b"hi back".to_vec()));
    }

    #[tokio::test]
    async fn send_to_unknown_peer_errors() {
        let mut t = UdpTransport::new(
            PeerId::from_bytes([9; 8]),
            UdpConfig {
                multicast: false,
                ..Default::default()
            },
        );
        t.start().await.unwrap();
        assert!(t.send(&PeerId::from_bytes([8; 8]), b"x").await.is_err());
    }
}
