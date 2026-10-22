use amara_core::capability::Availability;
use amara_core::peer::{PeerId, PeerInfo};
use dashmap::DashMap;
use std::sync::Arc;

/// Thread-safe peer registry for tracking known nodes in the mesh.
#[derive(Clone)]
pub struct PeerRegistry {
    peers: Arc<DashMap<PeerId, PeerInfo>>,
}

impl PeerRegistry {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(DashMap::new()),
        }
    }

    /// Insert or update a peer in the registry.
    pub fn update_peer(&self, peer_id: PeerId, info: PeerInfo) {
        self.peers.insert(peer_id, info);
    }

    /// Retrieve a peer by ID.
    pub fn get_peer(&self, peer_id: &PeerId) -> Option<PeerInfo> {
        self.peers.get(peer_id).map(|r| r.value().clone())
    }

    /// Remove a peer from the registry.
    pub fn remove_peer(&self, peer_id: &PeerId) {
        self.peers.remove(peer_id);
    }

    /// Update just the last_seen timestamp for a peer.
    pub fn touch_peer(&self, peer_id: &PeerId) {
        if let Some(mut peer) = self.peers.get_mut(peer_id) {
            peer.touch();
        }
    }

    /// Get all known peers.
    pub fn all_peers(&self) -> Vec<PeerInfo> {
        self.peers.iter().map(|r| r.value().clone()).collect()
    }

    /// Resolve a user-supplied peer reference: full/prefix hex PeerId (>= 4 chars),
    /// exact display name, or unique display-name prefix (case-insensitive).
    pub fn resolve(&self, query: &str) -> Option<PeerInfo> {
        let q = query.trim().trim_start_matches('@').to_lowercase();
        if q.is_empty() {
            return None;
        }
        let peers = self.all_peers();
        if q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit()) {
            let by_id: Vec<_> = peers
                .iter()
                .filter(|p| p.peer_id.to_string().starts_with(&q))
                .collect();
            if by_id.len() == 1 {
                return Some(by_id[0].clone());
            }
        }
        if let Some(p) = peers.iter().find(|p| p.display_name.to_lowercase() == q) {
            return Some(p.clone());
        }
        let by_prefix: Vec<_> = peers
            .iter()
            .filter(|p| p.display_name.to_lowercase().starts_with(&q))
            .collect();
        (by_prefix.len() == 1).then(|| by_prefix[0].clone())
    }

    /// Number of known peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// True when no peers are known.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Find peers with a specific capability.
    pub fn find_by_capability(&self, capability: &str) -> Vec<PeerInfo> {
        self.peers
            .iter()
            .filter(|r| {
                r.capabilities.iter().any(|c| c == capability)
                    && r.availability != Availability::Offline
            })
            .map(|r| r.value().clone())
            .collect()
    }
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str, key: u8) -> PeerInfo {
        PeerInfo::new(name, [key; 32], [key; 32])
    }

    #[test]
    fn resolve_by_name_prefix_and_id() {
        let reg = PeerRegistry::new();
        let nova = peer("nova", 1);
        let id = nova.peer_id;
        reg.update_peer(id, nova);
        reg.update_peer(peer("orion", 2).peer_id, peer("orion", 2));
        reg.update_peer(peer("orbit", 3).peer_id, peer("orbit", 3));

        assert_eq!(reg.resolve("Nova").unwrap().peer_id, id);
        assert_eq!(reg.resolve("@no").unwrap().peer_id, id);
        assert_eq!(reg.resolve(&id.to_string()[..6]).unwrap().peer_id, id);
        assert!(reg.resolve("or").is_none(), "ambiguous prefix");
        assert!(reg.resolve("").is_none());
    }
}
