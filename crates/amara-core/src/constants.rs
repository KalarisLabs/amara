//! System-wide constants for the AMARA protocol and network.

/// Current protocol version.
pub const PROTOCOL_VERSION: u8 = 0x01;

/// Maximum TTL for packets.
pub const MAX_TTL: u8 = 7;

/// Default TTL for new packets.
pub const DEFAULT_TTL: u8 = 7;

/// Fixed header size in bytes.
pub const HEADER_SIZE: usize = 42;

/// Ed25519 signature size in bytes.
pub const SIGNATURE_SIZE: usize = 64;

/// PeerId size in bytes.
pub const PEER_ID_SIZE: usize = 8;

/// Ed25519 public key size in bytes.
pub const PUBLIC_KEY_SIZE: usize = 32;

/// Maximum packet size (16 MiB).
pub const MAX_PACKET_SIZE: usize = 16 * 1024 * 1024;

/// Default fragment payload size (500 - 12 byte fragment header).
pub const DEFAULT_FRAGMENT_PAYLOAD: usize = 488;

/// Maximum transport unit for BLE.
pub const BLE_MTU: usize = 500;

/// Maximum fragments per message.
pub const MAX_FRAGMENTS: u16 = 8192;

/// Fragment reassembly timeout in seconds.
pub const FRAGMENT_REASSEMBLY_TIMEOUT_SECS: u64 = 30;

/// Maximum concurrent fragment reassemblies.
pub const MAX_CONCURRENT_REASSEMBLIES: usize = 64;

/// Deduplication cache capacity.
pub const DEDUP_CACHE_SIZE: usize = 10_000;

/// Maximum capability document size in bytes (4 KiB).
pub const MAX_CAPABILITY_DOC_SIZE: usize = 4096;

/// Maximum display name length in bytes.
pub const MAX_DISPLAY_NAME_LENGTH: usize = 64;

/// Maximum task input size (8 MiB).
pub const MAX_TASK_INPUT_SIZE: usize = 8 * 1024 * 1024;

/// Maximum result data size (8 MiB).
pub const MAX_RESULT_SIZE: usize = 8 * 1024 * 1024;

/// Maximum stored messages for store-and-forward.
pub const MAX_STORED_MESSAGES: usize = 1000;

/// Store-and-forward TTL in seconds (12 hours).
pub const STORE_FORWARD_TTL_SECS: u64 = 12 * 3600;

/// Peer expiry timeout in seconds (5 minutes).
pub const PEER_EXPIRY_SECS: u64 = 300;

/// Heartbeat interval in seconds.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Announce interval in seconds.
pub const ANNOUNCE_INTERVAL_SECS: u64 = 15;

/// Minimum relay jitter in milliseconds.
pub const RELAY_JITTER_MIN_MS: u64 = 10;

/// Maximum relay jitter in milliseconds.
pub const RELAY_JITTER_MAX_MS: u64 = 100;

/// Rate limit: maximum packets per second per peer.
pub const RATE_LIMIT_PACKETS_PER_SEC: u32 = 100;

/// Rate limit: maximum tasks per minute per peer.
pub const RATE_LIMIT_TASKS_PER_MIN: u32 = 120;

/// Default local API port.
pub const DEFAULT_API_PORT: u16 = 7432;

/// Default local API bind address.
pub const DEFAULT_API_BIND: &str = "127.0.0.1:7432";

/// AMARA version string.
pub const AMARA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// LZ4 compression threshold in bytes.
pub const COMPRESSION_THRESHOLD: usize = 100;

/// Maximum delegation depth for tasks.
pub const MAX_DELEGATION_DEPTH: u8 = 5;

/// CRC-16 polynomial (CRC-16/IBM).
pub const CRC16_POLY: u16 = 0x8005;

/// Default UDP port for LAN mesh discovery (multicast).
pub const DEFAULT_MESH_PORT: u16 = 7433;

/// IPv4 multicast group used for LAN discovery ("AMR" in the last octets).
pub const MESH_MULTICAST_V4: [u8; 4] = [239, 77, 65, 82];

/// Safe UDP datagram payload size (fits a 1500-byte Ethernet/Wi-Fi MTU).
pub const UDP_MTU: usize = 1400;

/// Maximum accepted clock skew for non-announce packets (seconds).
pub const MAX_CLOCK_SKEW_SECS: u32 = 3600;

/// Default timeout for remote tool calls (seconds).
pub const DEFAULT_CALL_TIMEOUT_SECS: u64 = 60;
