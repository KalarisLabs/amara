//! Complete packet type with header, payload, and optional signature.

use crate::header::{PacketFlags, PacketHeader, PacketType};
use amara_core::peer::PeerId;
use amara_core::{AmaraError, Result, HEADER_SIZE, PROTOCOL_VERSION, SIGNATURE_SIZE};

/// A complete AMARA packet: header + payload + optional signature.
#[derive(Debug, Clone)]
pub struct Packet {
    pub header: PacketHeader,
    pub payload: Vec<u8>,
    pub signature: Option<[u8; SIGNATURE_SIZE]>,
}

impl Packet {
    /// Create a new packet with the given type and payload.
    pub fn new(
        packet_type: PacketType,
        source: PeerId,
        destination: PeerId,
        ttl: u8,
        payload: Vec<u8>,
    ) -> Self {
        let mut flags = PacketFlags::new();
        if packet_type.is_relay_eligible() {
            flags.set(PacketFlags::RELAY_ELIGIBLE);
        }

        let header = PacketHeader {
            version: PROTOCOL_VERSION,
            packet_type,
            flags,
            ttl,
            message_id: amara_core::types::random_message_id(),
            source_id: source,
            destination_id: destination,
            timestamp: amara_core::types::now_timestamp(),
            payload_length: payload.len() as u32,
            sequence: 0,
            reserved: 0,
            checksum: 0,
        };

        Self {
            header,
            payload,
            signature: None,
        }
    }

    /// Create a broadcast packet.
    pub fn broadcast(packet_type: PacketType, source: PeerId, ttl: u8, payload: Vec<u8>) -> Self {
        Self::new(packet_type, source, PeerId::broadcast(), ttl, payload)
    }

    /// Sign this packet with an Ed25519 signing key.
    pub fn sign(&mut self, signing_key: &ed25519_dalek::SigningKey) {
        let header_bytes = self.header.encode();
        let sig = amara_crypto::signing::sign_packet(signing_key, &header_bytes, &self.payload);
        self.signature = Some(sig);
        self.header.flags.set(PacketFlags::SIGNED);
    }

    /// Verify this packet's signature.
    pub fn verify(&self, verifying_key: &ed25519_dalek::VerifyingKey) -> bool {
        let Some(ref sig) = self.signature else {
            return false;
        };
        let header_bytes = self.header.encode();
        amara_crypto::signing::verify_packet(verifying_key, &header_bytes, &self.payload, sig)
    }

    /// Encode the full packet to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut header = self.header.clone();
        header.payload_length = self.payload.len() as u32;

        let header_bytes = header.encode();
        let sig_size = if self.signature.is_some() {
            SIGNATURE_SIZE
        } else {
            0
        };

        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len() + sig_size);
        buf.extend_from_slice(&header_bytes);
        buf.extend_from_slice(&self.payload);
        if let Some(ref sig) = self.signature {
            buf.extend_from_slice(sig);
        }

        buf
    }

    /// Decode a packet from bytes.
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER_SIZE {
            return Err(AmaraError::MalformedPacket(
                "data too short for header".into(),
            ));
        }

        let header = PacketHeader::decode(&data[..HEADER_SIZE])?;
        let payload_len = header.payload_length as usize;

        let expected_len = if header.flags.is_signed() {
            HEADER_SIZE + payload_len + SIGNATURE_SIZE
        } else {
            HEADER_SIZE + payload_len
        };

        if data.len() < expected_len {
            return Err(AmaraError::MalformedPacket(format!(
                "data too short: {} bytes (expected {})",
                data.len(),
                expected_len
            )));
        }

        let payload = data[HEADER_SIZE..HEADER_SIZE + payload_len].to_vec();

        let signature = if header.flags.is_signed() {
            let sig_start = HEADER_SIZE + payload_len;
            let mut sig = [0u8; SIGNATURE_SIZE];
            sig.copy_from_slice(&data[sig_start..sig_start + SIGNATURE_SIZE]);
            Some(sig)
        } else {
            None
        };

        Ok(Self {
            header,
            payload,
            signature,
        })
    }

    /// Total size of the encoded packet.
    pub fn wire_size(&self) -> usize {
        HEADER_SIZE
            + self.payload.len()
            + if self.signature.is_some() {
                SIGNATURE_SIZE
            } else {
                0
            }
    }

    /// Check if this packet should be relayed.
    pub fn should_relay(&self) -> bool {
        self.header.flags.is_relay_eligible() && self.header.ttl > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amara_core::DEFAULT_TTL;

    #[test]
    fn packet_encode_decode_roundtrip() {
        let source = PeerId::from_bytes([1; 8]);
        let dest = PeerId::from_bytes([2; 8]);

        let original = Packet::new(
            PacketType::Task,
            source,
            dest,
            DEFAULT_TTL,
            b"test payload".to_vec(),
        );

        let encoded = original.encode();
        let decoded = Packet::decode(&encoded).unwrap();

        assert_eq!(decoded.header.packet_type, PacketType::Task);
        assert_eq!(decoded.header.source_id, source);
        assert_eq!(decoded.header.destination_id, dest);
        assert_eq!(decoded.payload, b"test payload");
    }

    #[test]
    fn packet_sign_verify() {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let key = SigningKey::generate(&mut OsRng);
        let source = PeerId::from_public_key(&key.verifying_key().to_bytes());

        let mut pkt = Packet::new(
            PacketType::Announce,
            source,
            PeerId::broadcast(),
            7,
            b"hello".to_vec(),
        );

        pkt.sign(&key);
        assert!(pkt.header.flags.is_signed());
        assert!(pkt.verify(&key.verifying_key()));

        // Roundtrip through encode/decode
        let encoded = pkt.encode();
        let decoded = Packet::decode(&encoded).unwrap();
        assert!(decoded.verify(&key.verifying_key()));
    }

    #[test]
    fn broadcast_packet() {
        let source = PeerId::from_bytes([5; 8]);
        let pkt = Packet::broadcast(PacketType::Announce, source, 7, vec![]);
        assert!(pkt.header.destination_id.is_broadcast());
    }

    #[test]
    fn relay_eligibility() {
        let source = PeerId::from_bytes([1; 8]);

        let announce = Packet::broadcast(PacketType::Announce, source, 7, vec![]);
        assert!(announce.should_relay());

        let ping = Packet::new(
            PacketType::Ping,
            source,
            PeerId::from_bytes([2; 8]),
            7,
            vec![],
        );
        assert!(!ping.should_relay()); // Ping is not relay-eligible
    }

    #[test]
    fn decode_truncated_fails() {
        let result = Packet::decode(&[0u8; 20]);
        assert!(result.is_err());
    }
}
