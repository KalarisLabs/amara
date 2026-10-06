//! Shared types used across the AMARA system.

use serde::{Deserialize, Serialize};

/// Timestamp type used throughout AMARA (Unix seconds, u32).
pub type Timestamp = u32;

/// Message identifier (8 bytes, randomly generated).
pub type MessageId = [u8; 8];

/// Generate a random message ID.
pub fn random_message_id() -> MessageId {
    let mut id = [0u8; 8];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut id);
    id
}

/// Generate a random message ID using a specific RNG (for deterministic testing).
pub fn random_message_id_with_rng(rng: &mut impl rand::RngCore) -> MessageId {
    let mut id = [0u8; 8];
    rng.fill_bytes(&mut id);
    id
}

/// Get the current Unix timestamp as u32.
pub fn now_timestamp() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32
}

/// Resource metadata advertised by a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceMetadata {
    /// CPU class (low, medium, high).
    #[serde(default)]
    pub cpu_class: ResourceClass,
    /// Memory class.
    #[serde(default)]
    pub memory_class: ResourceClass,
    /// Whether a GPU is present.
    #[serde(default)]
    pub gpu_present: bool,
    /// Battery state (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_state: Option<BatteryState>,
    /// Current load percentage (0-100).
    #[serde(default)]
    pub load_percent: u8,
}

/// Coarse resource classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceClass {
    Low,
    #[default]
    Medium,
    High,
}

/// Battery state for mobile/edge devices.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BatteryState {
    /// Battery percentage (0-100).
    pub percent: u8,
    /// Whether the device is currently charging.
    pub charging: bool,
}

/// Event types emitted by the AMARA system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AmaraEvent {
    PeerJoined { peer_id: String, name: String },
    PeerLeft { peer_id: String, name: String },
    CapabilityChanged { peer_id: String },
    TaskCreated { task_id: String, capability: String },
    TaskRouted { task_id: String, target: String },
    TaskForwarded { task_id: String, relay: String },
    TaskAccepted { task_id: String, agent: String },
    TaskCompleted { task_id: String, agent: String },
    TaskFailed { task_id: String, error: String },
    PacketDropped { reason: String },
    RouteChanged { peer_id: String, hops: u8 },
    SecurityEvent { event: String, peer_id: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_uniqueness() {
        let id1 = random_message_id();
        let id2 = random_message_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn deterministic_message_id() {
        use rand::SeedableRng;
        let mut rng1 = rand_chacha::ChaCha20Rng::seed_from_u64(42);
        let mut rng2 = rand_chacha::ChaCha20Rng::seed_from_u64(42);
        let id1 = random_message_id_with_rng(&mut rng1);
        let id2 = random_message_id_with_rng(&mut rng2);
        assert_eq!(id1, id2);
    }

    #[test]
    fn timestamp_is_recent() {
        let ts = now_timestamp();
        // Should be after 2024-01-01
        assert!(ts > 1704067200);
    }
}
