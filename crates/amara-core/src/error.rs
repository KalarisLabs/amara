//! Error types for the AMARA system.

use thiserror::Error;

/// The canonical error type for AMARA operations.
#[derive(Debug, Error)]
pub enum AmaraError {
    // Protocol errors
    #[error("malformed packet: {0}")]
    MalformedPacket(String),

    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u8),

    #[error("packet too large: {size} bytes (max {max})")]
    PacketTooLarge { size: usize, max: usize },

    #[error("invalid signature from peer {peer}")]
    InvalidSignature { peer: String },

    #[error("packet expired")]
    Expired,

    #[error("duplicate packet (message_id already seen)")]
    Duplicate,

    // Fragment errors
    #[error("fragment reassembly error: {0}")]
    FragmentError(String),

    #[error("too many concurrent reassemblies ({0})")]
    TooManyReassemblies(usize),

    #[error("fragment index {index} exceeds total {total}")]
    FragmentIndexOutOfRange { index: u16, total: u16 },

    // Transport errors
    #[error("transport error: {0}")]
    Transport(String),

    #[error("BLE error: {0}")]
    Ble(String),

    #[error("no transport available")]
    NoTransport,

    // Crypto errors
    #[error("encryption error: {0}")]
    Encryption(String),

    #[error("decryption error: {0}")]
    Decryption(String),

    #[error("key exchange error: {0}")]
    KeyExchange(String),

    // Peer errors
    #[error("unknown peer: {0}")]
    UnknownPeer(String),

    #[error("peer blocked: {0}")]
    PeerBlocked(String),

    #[error("rate limited by peer {0}")]
    RateLimited(String),

    // Task errors
    #[error("task not found: {0}")]
    TaskNotFound(String),

    #[error("capability not available: {0}")]
    CapabilityNotAvailable(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("agent overloaded")]
    Overloaded,

    #[error("task loop detected (depth {depth})")]
    TaskLoop { depth: u8 },

    #[error("task expired")]
    TaskExpired,

    #[error("timed out: {0}")]
    Timeout(String),

    // IO / system errors
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("configuration error: {0}")]
    Config(String),

    // Generic
    #[error("{0}")]
    Other(String),
}

/// Convenience Result type using AmaraError.
pub type Result<T> = std::result::Result<T, AmaraError>;

/// Error code for protocol-level error responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ErrorCode {
    UnknownPeer = 0x01,
    UnsupportedVersion = 0x02,
    InvalidSignature = 0x03,
    Expired = 0x04,
    RateLimited = 0x05,
    CapabilityMissing = 0x06,
    TaskNotFound = 0x07,
    PermissionDenied = 0x08,
    Overloaded = 0x09,
    FragmentError = 0x0A,
    SizeExceeded = 0x0B,
    Malformed = 0x0C,
}

impl ErrorCode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::UnknownPeer),
            0x02 => Some(Self::UnsupportedVersion),
            0x03 => Some(Self::InvalidSignature),
            0x04 => Some(Self::Expired),
            0x05 => Some(Self::RateLimited),
            0x06 => Some(Self::CapabilityMissing),
            0x07 => Some(Self::TaskNotFound),
            0x08 => Some(Self::PermissionDenied),
            0x09 => Some(Self::Overloaded),
            0x0A => Some(Self::FragmentError),
            0x0B => Some(Self::SizeExceeded),
            0x0C => Some(Self::Malformed),
            _ => None,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::UnknownPeer => "Peer not found in network",
            Self::UnsupportedVersion => "Protocol version not supported",
            Self::InvalidSignature => "Packet signature verification failed",
            Self::Expired => "Packet or task has expired",
            Self::RateLimited => "Too many requests from this peer",
            Self::CapabilityMissing => "Requested capability is not available",
            Self::TaskNotFound => "Referenced task does not exist",
            Self::PermissionDenied => "Trust or policy check failed",
            Self::Overloaded => "Agent is at capacity, try later",
            Self::FragmentError => "Fragment reassembly failed",
            Self::SizeExceeded => "Payload exceeds size limit",
            Self::Malformed => "Packet could not be parsed",
        }
    }
}
