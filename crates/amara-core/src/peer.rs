//! Peer identity and metadata types.

use serde::{Deserialize, Serialize};
use std::fmt;

/// 8-byte peer identifier derived from BLAKE3 hash of the Ed25519 public key.
///
/// PeerIds are the canonical way to address nodes in the AMARA network.
/// They are never derived from IP or MAC addresses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub [u8; 8]);

impl PeerId {
    /// Create a PeerId from raw bytes.
    pub fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Create a PeerId from a public key by hashing with BLAKE3 and truncating.
    pub fn from_public_key(public_key: &[u8; 32]) -> Self {
        let hash = blake3::hash(public_key);
        let mut id = [0u8; 8];
        id.copy_from_slice(&hash.as_bytes()[..8]);
        Self(id)
    }

    /// The broadcast address (all zeros). Used for broadcast/multicast packets.
    pub fn broadcast() -> Self {
        Self([0u8; 8])
    }

    /// Check if this is the broadcast address.
    pub fn is_broadcast(&self) -> bool {
        self.0 == [0u8; 8]
    }

    /// Return the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    /// Return a short hex string (first 4 bytes) for display.
    pub fn short_hex(&self) -> String {
        hex::encode(&self.0[..4])
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({})", self.short_hex())
    }
}

/// Trust level assigned to a peer by the local node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TrustLevel {
    /// Newly discovered, no interaction yet.
    Unknown = 0,
    /// Exchanged announcements, signature verified.
    Seen = 1,
    /// Completed key exchange, identity authenticated.
    Verified = 2,
    /// Explicitly trusted by the user.
    Trusted = 3,
    /// Explicitly blocked; all traffic dropped.
    Blocked = 255,
}

impl Default for TrustLevel {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Platform identifier for the peer's operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Platform {
    Unknown = 0,
    Linux = 1,
    MacOS = 2,
    Windows = 3,
    RaspberryPi = 4,
    Android = 5,
    IOS = 6,
}

impl Platform {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Linux,
            2 => Self::MacOS,
            3 => Self::Windows,
            4 => Self::RaspberryPi,
            5 => Self::Android,
            6 => Self::IOS,
            _ => Self::Unknown,
        }
    }

    /// Detect the current platform at runtime.
    pub fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            // Check if we're on a Raspberry Pi
            if std::path::Path::new("/proc/device-tree/model").exists() {
                if let Ok(model) = std::fs::read_to_string("/proc/device-tree/model") {
                    if model.contains("Raspberry Pi") {
                        return Self::RaspberryPi;
                    }
                }
            }
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOS
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Self::Unknown
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Linux => "linux",
            Self::MacOS => "mac",
            Self::Windows => "windows",
            Self::RaspberryPi => "raspberry-pi",
            Self::Android => "android",
            Self::IOS => "ios",
        }
    }
}

/// Full information about a known peer in the local registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Cryptographic peer identifier.
    pub peer_id: PeerId,
    /// Human-readable display name.
    pub display_name: String,
    /// Ed25519 public key (32 bytes).
    pub public_key: [u8; 32],
    /// Advertised capabilities.
    pub capabilities: Vec<String>,
    /// Estimated hop count.
    pub hop_count: u8,
    /// When this peer was last seen.
    pub last_seen: chrono::DateTime<chrono::Utc>,
    /// Current trust level.
    pub trust_level: TrustLevel,
    /// Which transport discovered this peer.
    pub transport: String,
    /// Peer's reported load.
    pub load_class: super::capability::LoadClass,
    /// Peer's reported availability.
    pub availability: super::capability::Availability,
    /// Protocol version the peer is running.
    pub version: String,
    /// Platform/OS identifier.
    pub platform: Platform,
    /// Next-hop peer for routing (None if direct).
    pub next_hop: Option<PeerId>,
}

impl PeerInfo {
    /// Check if this peer entry has expired.
    pub fn is_expired(&self, timeout: chrono::Duration) -> bool {
        chrono::Utc::now() - self.last_seen > timeout
    }

    /// Update the last_seen timestamp to now.
    pub fn touch(&mut self) {
        self.last_seen = chrono::Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_from_public_key_is_deterministic() {
        let key = [42u8; 32];
        let id1 = PeerId::from_public_key(&key);
        let id2 = PeerId::from_public_key(&key);
        assert_eq!(id1, id2);
    }

    #[test]
    fn peer_id_display() {
        let id = PeerId::from_bytes([0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89]);
        assert_eq!(id.to_string(), "abcdef0123456789");
        assert_eq!(id.short_hex(), "abcdef01");
    }

    #[test]
    fn broadcast_peer_id() {
        let b = PeerId::broadcast();
        assert!(b.is_broadcast());
        let non_b = PeerId::from_bytes([1; 8]);
        assert!(!non_b.is_broadcast());
    }

    #[test]
    fn platform_detection() {
        let p = Platform::current();
        #[cfg(target_os = "windows")]
        assert_eq!(p, Platform::Windows);
        #[cfg(target_os = "macos")]
        assert_eq!(p, Platform::MacOS);
        #[cfg(target_os = "linux")]
        assert!(p == Platform::Linux || p == Platform::RaspberryPi);
    }
}
