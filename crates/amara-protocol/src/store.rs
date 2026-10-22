//! Store-and-forward envelope carried by `STORE` and `FORWARD` packets.
//!
//! ```text
//! envelope = ed25519_public (32) · x25519_public (32) · name (u8 len · bytes) · packet
//! ```
//!
//! `packet` is the original, fully signed packet (usually a sealed direct
//! message). The keys travel with it so the recipient can verify the packet
//! and answer the sender even if the sender is long gone: the envelope is
//! self-certifying, like an announce (PeerId = BLAKE3(ed25519_public)).

use crate::packet::Packet;
use amara_core::peer::PeerId;
use amara_core::{AmaraError, MAX_DISPLAY_NAME_LENGTH, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    /// Sender display name (unauthenticated label).
    pub name: String,
    /// The encoded, signed inner packet.
    pub packet: Vec<u8>,
}

impl Envelope {
    pub fn encode(&self) -> Vec<u8> {
        let mut end = self.name.len().min(MAX_DISPLAY_NAME_LENGTH);
        while !self.name.is_char_boundary(end) {
            end -= 1;
        }
        let mut buf = Vec::with_capacity(65 + end + self.packet.len());
        buf.extend_from_slice(&self.ed25519_public);
        buf.extend_from_slice(&self.x25519_public);
        buf.push(end as u8);
        buf.extend_from_slice(&self.name.as_bytes()[..end]);
        buf.extend_from_slice(&self.packet);
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 65 {
            return Err(AmaraError::MalformedPacket("envelope too short".into()));
        }
        let mut ed25519_public = [0u8; 32];
        ed25519_public.copy_from_slice(&bytes[..32]);
        let mut x25519_public = [0u8; 32];
        x25519_public.copy_from_slice(&bytes[32..64]);
        let len = bytes[64] as usize;
        let name = bytes
            .get(65..65 + len)
            .ok_or_else(|| AmaraError::MalformedPacket("envelope name truncated".into()))?;
        Ok(Self {
            ed25519_public,
            x25519_public,
            name: String::from_utf8_lossy(name).into_owned(),
            packet: bytes[65 + len..].to_vec(),
        })
    }

    /// PeerId of the keys in this envelope.
    pub fn sender(&self) -> PeerId {
        PeerId::from_public_key(&self.ed25519_public)
    }

    /// Decode the inner packet and check it is signed by the envelope's keys.
    pub fn open(&self) -> Result<Packet> {
        let packet = Packet::decode(&self.packet)?;
        let invalid = || AmaraError::InvalidSignature {
            peer: packet.header.source_id.to_string(),
        };
        if packet.header.source_id != self.sender() {
            return Err(invalid());
        }
        let key =
            ed25519_dalek::VerifyingKey::from_bytes(&self.ed25519_public).map_err(|_| invalid())?;
        if !packet.verify(&key) {
            return Err(invalid());
        }
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::PacketType;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn signed(key: &SigningKey) -> Packet {
        let source = PeerId::from_public_key(&key.verifying_key().to_bytes());
        let mut p = Packet::new(
            PacketType::Message,
            source,
            PeerId::from_bytes([9; 8]),
            7,
            b"sealed".to_vec(),
        );
        p.sign(key);
        p
    }

    #[test]
    fn roundtrip_and_verify() {
        let key = SigningKey::generate(&mut OsRng);
        let env = Envelope {
            ed25519_public: key.verifying_key().to_bytes(),
            x25519_public: [3; 32],
            name: "alice".into(),
            packet: signed(&key).encode(),
        };
        let decoded = Envelope::decode(&env.encode()).unwrap();
        assert_eq!(decoded, env);
        assert_eq!(decoded.open().unwrap().payload, b"sealed");
    }

    #[test]
    fn foreign_keys_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let other = SigningKey::generate(&mut OsRng);
        let env = Envelope {
            ed25519_public: other.verifying_key().to_bytes(),
            x25519_public: [0; 32],
            name: String::new(),
            packet: signed(&key).encode(),
        };
        assert!(env.open().is_err());
        for cut in 0..70 {
            let _ = Envelope::decode(&env.encode()[..cut]); // must never panic
        }
    }
}
