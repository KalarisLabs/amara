//! High-level codec for encoding and decoding application-level messages.
//!
//! This module bridges the gap between typed application messages (announce,
//! task, result, etc.) and the raw binary protocol.

use amara_core::capability::CapabilityDocument;
use amara_core::task::{Task, TaskResult};
use amara_core::{AmaraError, Result};

/// Encode a capability document for wire transmission.
pub fn encode_capability(doc: &CapabilityDocument) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(doc).map_err(|e| AmaraError::Serialization(e.to_string()))?;

    if json.len() > amara_core::MAX_CAPABILITY_DOC_SIZE {
        return Err(AmaraError::PacketTooLarge {
            size: json.len(),
            max: amara_core::MAX_CAPABILITY_DOC_SIZE,
        });
    }

    // Capability payload: [2 bytes json_length] [json] [4 bytes version] [8 bytes hash]
    let cap_version: u32 = 1;
    let hash = blake3::hash(&json);
    let hash_truncated = &hash.as_bytes()[..8];

    let mut payload = Vec::with_capacity(2 + json.len() + 4 + 8);
    payload.extend_from_slice(&(json.len() as u16).to_be_bytes());
    payload.extend_from_slice(&json);
    payload.extend_from_slice(&cap_version.to_be_bytes());
    payload.extend_from_slice(hash_truncated);

    Ok(payload)
}

/// Decode a capability document from wire format.
pub fn decode_capability(payload: &[u8]) -> Result<(CapabilityDocument, u32, [u8; 8])> {
    if payload.len() < 2 {
        return Err(AmaraError::MalformedPacket(
            "capability payload too short".into(),
        ));
    }

    let json_len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    if payload.len() < 2 + json_len + 12 {
        return Err(AmaraError::MalformedPacket(
            "capability payload truncated".into(),
        ));
    }

    let json_data = &payload[2..2 + json_len];
    let doc: CapabilityDocument = serde_json::from_slice(json_data)
        .map_err(|e| AmaraError::Serialization(format!("invalid capability JSON: {e}")))?;

    let offset = 2 + json_len;
    let version = u32::from_be_bytes([
        payload[offset],
        payload[offset + 1],
        payload[offset + 2],
        payload[offset + 3],
    ]);

    let mut hash = [0u8; 8];
    hash.copy_from_slice(&payload[offset + 4..offset + 12]);

    // Verify hash
    let computed = blake3::hash(json_data);
    let expected: [u8; 8] = computed.as_bytes()[..8].try_into().unwrap();
    if hash != expected {
        return Err(AmaraError::MalformedPacket(
            "capability hash mismatch".into(),
        ));
    }

    Ok((doc, version, hash))
}

/// Encode a task for wire transmission.
pub fn encode_task(task: &Task) -> Result<Vec<u8>> {
    serde_json::to_vec(task).map_err(|e| AmaraError::Serialization(e.to_string()))
}

/// Decode a task from wire format.
pub fn decode_task(payload: &[u8]) -> Result<Task> {
    serde_json::from_slice(payload)
        .map_err(|e| AmaraError::Serialization(format!("invalid task: {e}")))
}

/// Encode a task result for wire transmission.
pub fn encode_result(result: &TaskResult) -> Result<Vec<u8>> {
    serde_json::to_vec(result).map_err(|e| AmaraError::Serialization(e.to_string()))
}

/// Decode a task result from wire format.
pub fn decode_result(payload: &[u8]) -> Result<TaskResult> {
    serde_json::from_slice(payload)
        .map_err(|e| AmaraError::Serialization(format!("invalid result: {e}")))
}

/// Maximum length of any short string field in an announce (bytes).
const MAX_ANNOUNCE_STR: usize = 64;
/// Maximum number of capabilities/tools listed in an announce.
const MAX_ANNOUNCE_CAPS: usize = 64;

/// Presence beacon. Self-certifying: the PeerId is BLAKE3(public_key), so any
/// node can check the announce was produced by the owner of that PeerId.
#[derive(Debug, Clone, PartialEq)]
pub struct Announce {
    /// Ed25519 public key (verifies every packet from this peer).
    pub public_key: [u8; 32],
    /// X25519 public key (target of end-to-end sealed payloads).
    pub x25519_public: [u8; 32],
    pub name: String,
    pub version: String,
    pub platform: amara_core::peer::Platform,
    pub load: amara_core::capability::LoadClass,
    pub availability: amara_core::capability::Availability,
    /// Capability tags and tool names this agent serves.
    pub capabilities: Vec<String>,
}

fn push_str(buf: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(MAX_ANNOUNCE_STR);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    buf.push(end as u8);
    buf.extend_from_slice(&s.as_bytes()[..end]);
}

fn read_str(payload: &[u8], offset: &mut usize, what: &str) -> Result<String> {
    let len = *payload
        .get(*offset)
        .ok_or_else(|| AmaraError::MalformedPacket(format!("announce {what} missing")))?
        as usize;
    *offset += 1;
    let bytes = payload
        .get(*offset..*offset + len)
        .ok_or_else(|| AmaraError::MalformedPacket(format!("announce {what} truncated")))?;
    *offset += len;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

impl Announce {
    /// Encode as `pk(32) xpk(32) name ver platform load avail count caps..`
    /// where strings are u8-length-prefixed.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(96 + self.capabilities.len() * 12);
        buf.extend_from_slice(&self.public_key);
        buf.extend_from_slice(&self.x25519_public);
        push_str(&mut buf, &self.name);
        push_str(&mut buf, &self.version);
        buf.push(self.platform as u8);
        buf.push(self.load.to_u8());
        buf.push(self.availability.to_u8());
        let caps: Vec<&String> = self.capabilities.iter().take(MAX_ANNOUNCE_CAPS).collect();
        buf.push(caps.len() as u8);
        for cap in caps {
            push_str(&mut buf, cap);
        }
        buf
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() < 64 {
            return Err(AmaraError::MalformedPacket("announce too short".into()));
        }
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&payload[..32]);
        let mut x25519_public = [0u8; 32];
        x25519_public.copy_from_slice(&payload[32..64]);
        let mut offset = 64;
        let name = read_str(payload, &mut offset, "name")?;
        let version = read_str(payload, &mut offset, "version")?;
        let meta = payload
            .get(offset..offset + 4)
            .ok_or_else(|| AmaraError::MalformedPacket("announce metadata truncated".into()))?;
        let platform = amara_core::peer::Platform::from_u8(meta[0]);
        let load = amara_core::capability::LoadClass::from_u8(meta[1]);
        let availability = amara_core::capability::Availability::from_u8(meta[2]);
        let count = (meta[3] as usize).min(MAX_ANNOUNCE_CAPS);
        offset += 4;
        let mut capabilities = Vec::with_capacity(count);
        for _ in 0..count {
            capabilities.push(read_str(payload, &mut offset, "capability")?);
        }
        Ok(Self {
            public_key,
            x25519_public,
            name,
            version,
            platform,
            load,
            availability,
            capabilities,
        })
    }

    /// The PeerId this announce speaks for.
    pub fn peer_id(&self) -> amara_core::peer::PeerId {
        amara_core::peer::PeerId::from_public_key(&self.public_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amara_core::capability::*;

    #[test]
    fn capability_encode_decode_roundtrip() {
        let doc = CapabilityDocument {
            name: "test-agent".to_string(),
            kind: "agent".to_string(),
            capabilities: vec!["vision".to_string(), "ocr".to_string()],
            models: vec![],
            tools: vec!["ocr".to_string()],
            limits: ResourceLimits::default(),
            availability: Availability::Online,
        };

        let encoded = encode_capability(&doc).unwrap();
        let (decoded, version, _hash) = decode_capability(&encoded).unwrap();

        assert_eq!(decoded.name, "test-agent");
        assert_eq!(decoded.capabilities.len(), 2);
        assert_eq!(version, 1);
    }

    #[test]
    fn announce_encode_decode_roundtrip() {
        let announce = Announce {
            public_key: [7; 32],
            x25519_public: [9; 32],
            name: "nova".into(),
            version: "0.2.0".into(),
            platform: amara_core::peer::Platform::Linux,
            load: LoadClass::Low,
            availability: Availability::Online,
            capabilities: vec!["reasoning".into(), "summarize".into()],
        };
        let decoded = Announce::decode(&announce.encode()).unwrap();
        assert_eq!(decoded, announce);
        assert_eq!(
            decoded.peer_id(),
            amara_core::peer::PeerId::from_public_key(&[7; 32])
        );
    }

    #[test]
    fn announce_truncated_and_oversized_fields() {
        let mut announce = Announce {
            public_key: [1; 32],
            x25519_public: [2; 32],
            name: "\u{e9}".repeat(100),
            version: "v".into(),
            platform: amara_core::peer::Platform::Windows,
            load: LoadClass::Idle,
            availability: Availability::Online,
            capabilities: vec![],
        };
        let encoded = announce.encode();
        let decoded = Announce::decode(&encoded).unwrap();
        assert!(decoded.name.len() <= 64 && decoded.name.chars().all(|c| c == '\u{e9}'));
        for cut in 0..encoded.len() {
            let _ = Announce::decode(&encoded[..cut]); // must never panic
        }
        announce.name = String::new();
        assert!(Announce::decode(&announce.encode()).is_ok());
    }

    #[test]
    fn task_encode_decode_roundtrip() {
        let issuer = amara_core::peer::PeerId::from_bytes([1; 8]);
        let task = amara_core::task::Task::new(
            issuer,
            "vision",
            serde_json::json!({"prompt": "describe this image"}),
        );

        let encoded = encode_task(&task).unwrap();
        let decoded = decode_task(&encoded).unwrap();

        assert_eq!(decoded.task_id, task.task_id);
        assert_eq!(decoded.capability, "vision");
    }
}
