use amara_core::peer::{PeerId, PeerInfo, Platform, TrustLevel};
use dashmap::DashMap;
use std::sync::Arc;
use amara_core::capability::{Availability, LoadClass};

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

    /// Find peers with a specific capability.
    pub fn find_by_capability(&self, capability: &str) -> Vec<PeerInfo> {
        self.peers
            .iter()
            .filter(|r| r.capabilities.iter().any(|c| c == capability) && r.availability != Availability::Offline)
            .map(|r| r.value().clone())
            .collect()
    }
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
