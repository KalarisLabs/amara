//! Packet header definition and parsing.
//!
//! The AMARA packet header is a fixed 42-byte structure present on every packet.
//! All multi-byte integers are big-endian.

use amara_core::peer::PeerId;
use amara_core::{AmaraError, HEADER_SIZE, MAX_PACKET_SIZE, PROTOCOL_VERSION, Result};

/// Packet type identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PacketType {
    // Discovery
    Announce = 0x01,
    Capability = 0x02,
    Ping = 0x03,
    Pong = 0x04,

    // Task lifecycle
    Task = 0x10,
    TaskAccept = 0x11,
    TaskReject = 0x12,
    TaskCancel = 0x13,

    // Results
    Result = 0x20,
    ResultChunk = 0x21,
    ResultComplete = 0x22,

    // Events
    Event = 0x30,
    Heartbeat = 0x31,
    /// Human/agent message (direct or broadcast channel).
    Message = 0x32,

    // Control
    Ack = 0x40,
    Error = 0x41,

    // Routing
    RouteRequest = 0x50,
    RouteResponse = 0x51,

    // Store & forward
    Store = 0x60,
    Forward = 0x61,

    // File transfer
    FileManifest = 0x70,
    FileChunk = 0x71,
    FileComplete = 0x72,

    // Encryption
    KeyExchange = 0x80,
    Encrypted = 0x81,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Announce),
            0x02 => Some(Self::Capability),
            0x03 => Some(Self::Ping),
            0x04 => Some(Self::Pong),
            0x10 => Some(Self::Task),
            0x11 => Some(Self::TaskAccept),
            0x12 => Some(Self::TaskReject),
            0x13 => Some(Self::TaskCancel),
            0x20 => Some(Self::Result),
            0x21 => Some(Self::ResultChunk),
            0x22 => Some(Self::ResultComplete),
            0x30 => Some(Self::Event),
            0x31 => Some(Self::Heartbeat),
            0x32 => Some(Self::Message),
            0x40 => Some(Self::Ack),
            0x41 => Some(Self::Error),
            0x50 => Some(Self::RouteRequest),
            0x51 => Some(Self::RouteResponse),
            0x60 => Some(Self::Store),
            0x61 => Some(Self::Forward),
            0x70 => Some(Self::FileManifest),
            0x71 => Some(Self::FileChunk),
            0x72 => Some(Self::FileComplete),
            0x80 => Some(Self::KeyExchange),
            0x81 => Some(Self::Encrypted),
            _ => None,
        }
    }

    /// Check if this packet type is eligible for relay by default.
    pub fn is_relay_eligible(&self) -> bool {
        matches!(
            self,
            Self::Announce
                | Self::Capability
                | Self::Heartbeat
                | Self::Message
                | Self::Task
                | Self::Result
                | Self::ResultChunk
                | Self::ResultComplete
                | Self::Encrypted
                | Self::Store
                | Self::Forward
                | Self::RouteRequest
        )
    }

    /// Check if this is a broadcast packet type.
    pub fn is_broadcast(&self) -> bool {
        matches!(
            self,
            Self::Announce | Self::Capability | Self::Heartbeat | Self::Event | Self::RouteRequest
        )
    }
}

/// Packet header flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct PacketFlags(pub u8);

impl PacketFlags {
    pub const ENCRYPTED: u8 = 0b0000_0001;
    pub const FRAGMENTED: u8 = 0b0000_0010;
    pub const RELAY_ELIGIBLE: u8 = 0b0000_0100;
    pub const ACK_REQUESTED: u8 = 0b0000_1000;
    pub const COMPRESSED: u8 = 0b0001_0000;
    pub const SIGNED: u8 = 0b0010_0000;
    pub const PRIORITY: u8 = 0b0100_0000;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn set(&mut self, flag: u8) {
        self.0 |= flag;
    }

    pub fn clear(&mut self, flag: u8) {
        self.0 &= !flag;
    }

    pub fn has(&self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn is_encrypted(&self) -> bool {
        self.has(Self::ENCRYPTED)
    }

    pub fn is_fragmented(&self) -> bool {
        self.has(Self::FRAGMENTED)
    }

    pub fn is_relay_eligible(&self) -> bool {
        self.has(Self::RELAY_ELIGIBLE)
    }

    pub fn is_ack_requested(&self) -> bool {
        self.has(Self::ACK_REQUESTED)
    }

    pub fn is_compressed(&self) -> bool {
        self.has(Self::COMPRESSED)
    }

    pub fn is_signed(&self) -> bool {
        self.has(Self::SIGNED)
    }

    pub fn is_priority(&self) -> bool {
        self.has(Self::PRIORITY)
    }
}

/// 42-byte fixed packet header.
///
/// ```text
/// Offset  Size  Field
/// 0       1     version
/// 1       1     packet_type
/// 2       1     flags
/// 3       1     ttl
/// 4       8     message_id
/// 12      8     source_id
/// 20      8     destination_id
/// 28      4     timestamp
/// 32      4     payload_length
/// 36      2     sequence
/// 38      2     reserved
/// 40      2     checksum
/// ```
#[derive(Debug, Clone)]
pub struct PacketHeader {
    pub version: u8,
    pub packet_type: PacketType,
    pub flags: PacketFlags,
    pub ttl: u8,
    pub message_id: [u8; 8],
    pub source_id: PeerId,
    pub destination_id: PeerId,
    pub timestamp: u32,
    pub payload_length: u32,
    pub sequence: u16,
    pub reserved: u16,
    pub checksum: u16,
}

impl PacketHeader {
    /// Encode the header to a 42-byte buffer.
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];

        buf[0] = self.version;
        buf[1] = self.packet_type as u8;
        buf[2] = self.flags.0;
        buf[3] = self.ttl;
        buf[4..12].copy_from_slice(&self.message_id);
        buf[12..20].copy_from_slice(self.source_id.as_bytes());
        buf[20..28].copy_from_slice(self.destination_id.as_bytes());
        buf[28..32].copy_from_slice(&self.timestamp.to_be_bytes());
        buf[32..36].copy_from_slice(&self.payload_length.to_be_bytes());
        buf[36..38].copy_from_slice(&self.sequence.to_be_bytes());
        buf[38..40].copy_from_slice(&self.reserved.to_be_bytes());

        // Compute CRC-16 over bytes [0..40)
        let crc = compute_crc16(&buf[..40]);
        buf[40..42].copy_from_slice(&crc.to_be_bytes());

        buf
    }

    /// Decode a header from a byte slice (must be at least 42 bytes).
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_SIZE {
            return Err(AmaraError::MalformedPacket(format!(
                "header too short: {} bytes (need {})",
                buf.len(),
                HEADER_SIZE
            )));
        }

        let version = buf[0];
        if version != PROTOCOL_VERSION {
            return Err(AmaraError::UnsupportedVersion(version));
        }

        let packet_type = PacketType::from_u8(buf[1]).ok_or_else(|| {
            AmaraError::MalformedPacket(format!("unknown packet type: 0x{:02x}", buf[1]))
        })?;

        let flags = PacketFlags(buf[2]);
        let ttl = buf[3];

        let mut message_id = [0u8; 8];
        message_id.copy_from_slice(&buf[4..12]);

        let mut source_bytes = [0u8; 8];
        source_bytes.copy_from_slice(&buf[12..20]);
        let source_id = PeerId::from_bytes(source_bytes);

        let mut dest_bytes = [0u8; 8];
        dest_bytes.copy_from_slice(&buf[20..28]);
        let destination_id = PeerId::from_bytes(dest_bytes);

        let timestamp = u32::from_be_bytes([buf[28], buf[29], buf[30], buf[31]]);
        let payload_length = u32::from_be_bytes([buf[32], buf[33], buf[34], buf[35]]);
        let sequence = u16::from_be_bytes([buf[36], buf[37]]);
        let reserved = u16::from_be_bytes([buf[38], buf[39]]);
        let checksum = u16::from_be_bytes([buf[40], buf[41]]);

        // Verify CRC-16
        let computed_crc = compute_crc16(&buf[..40]);
        if computed_crc != checksum {
            return Err(AmaraError::MalformedPacket(format!(
                "CRC mismatch: expected 0x{:04x}, got 0x{:04x}",
                checksum, computed_crc
            )));
        }

        // Validate payload length
        if payload_length as usize > MAX_PACKET_SIZE {
            return Err(AmaraError::PacketTooLarge {
                size: payload_length as usize,
                max: MAX_PACKET_SIZE,
            });
        }

        Ok(Self {
            version,
            packet_type,
            flags,
            ttl,
            message_id,
            source_id,
            destination_id,
            timestamp,
            payload_length,
            sequence,
            reserved,
            checksum,
        })
    }

    /// Decrement TTL by 1 (for relay), returning false if TTL was already 0.
    pub fn decrement_ttl(&mut self) -> bool {
        if self.ttl == 0 {
            return false;
        }
        self.ttl -= 1;
        true
    }
}

/// Compute CRC-16 (IBM/ANSI variant) over a byte slice.
fn compute_crc16(data: &[u8]) -> u16 {
    use crc::{CRC_16_IBM_SDLC, Crc};
    const CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_SDLC);
    CRC.checksum(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amara_core::types::random_message_id;

    fn make_test_header() -> PacketHeader {
        let mut flags = PacketFlags::new();
        flags.set(PacketFlags::SIGNED);
        flags.set(PacketFlags::RELAY_ELIGIBLE);

        PacketHeader {
            version: PROTOCOL_VERSION,
            packet_type: PacketType::Announce,
            flags,
            ttl: 7,
            message_id: random_message_id(),
            source_id: PeerId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
            destination_id: PeerId::broadcast(),
            timestamp: 1700000000,
            payload_length: 128,
            sequence: 0,
            reserved: 0,
            checksum: 0, // Will be computed during encode
        }
    }

    #[test]
    fn header_encode_decode_roundtrip() {
        let header = make_test_header();
        let encoded = header.encode();
        assert_eq!(encoded.len(), HEADER_SIZE);

        let decoded = PacketHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.version, header.version);
        assert_eq!(decoded.packet_type, header.packet_type);
        assert_eq!(decoded.ttl, header.ttl);
        assert_eq!(decoded.message_id, header.message_id);
        assert_eq!(decoded.source_id, header.source_id);
        assert_eq!(decoded.destination_id, header.destination_id);
        assert_eq!(decoded.timestamp, header.timestamp);
        assert_eq!(decoded.payload_length, header.payload_length);
    }

    #[test]
    fn header_crc_validation() {
        let header = make_test_header();
        let mut encoded = header.encode();

        // Tamper with a byte
        encoded[5] ^= 0xFF;

        let result = PacketHeader::decode(&encoded);
        assert!(result.is_err());
    }

    #[test]
    fn header_too_short() {
        let result = PacketHeader::decode(&[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_protocol_version() {
        let mut header = make_test_header();
        header.version = 0xFF;
        let mut encoded = header.encode();
        encoded[0] = 0xFF;

        let result = PacketHeader::decode(&encoded);
        assert!(matches!(result, Err(AmaraError::UnsupportedVersion(0xFF))));
    }

    #[test]
    fn flags_operations() {
        let mut flags = PacketFlags::new();
        assert!(!flags.is_encrypted());

        flags.set(PacketFlags::ENCRYPTED);
        assert!(flags.is_encrypted());

        flags.set(PacketFlags::COMPRESSED);
        assert!(flags.is_compressed());
        assert!(flags.is_encrypted());

        flags.clear(PacketFlags::ENCRYPTED);
        assert!(!flags.is_encrypted());
        assert!(flags.is_compressed());
    }

    #[test]
    fn ttl_decrement() {
        let mut header = make_test_header();
        header.ttl = 2;

        assert!(header.decrement_ttl());
        assert_eq!(header.ttl, 1);

        assert!(header.decrement_ttl());
        assert_eq!(header.ttl, 0);

        assert!(!header.decrement_ttl());
        assert_eq!(header.ttl, 0);
    }

    #[test]
    fn all_packet_types_parse() {
        let types = [
            0x01u8, 0x02, 0x03, 0x04, 0x10, 0x11, 0x12, 0x13, 0x20, 0x21, 0x22, 0x30, 0x31, 0x32,
            0x40, 0x41, 0x50, 0x51, 0x60, 0x61, 0x70, 0x71, 0x72, 0x80, 0x81,
        ];
        for t in types {
            assert!(
                PacketType::from_u8(t).is_some(),
                "type 0x{:02x} should parse",
                t
            );
        }

        // Reserved range should not parse
        assert!(PacketType::from_u8(0xF0).is_none());
        assert!(PacketType::from_u8(0xFF).is_none());
    }
}
