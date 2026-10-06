//! High-level codec for encoding and decoding application-level messages.
//!
//! This module bridges the gap between typed application messages (announce,
//! task, result, etc.) and the raw binary protocol.

use amara_core::{AmaraError, Result};
use amara_core::capability::CapabilityDocument;
use amara_core::task::{Task, TaskResult};

/// Encode a capability document for wire transmission.
pub fn encode_capability(doc: &CapabilityDocument) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(doc)
        .map_err(|e| AmaraError::Serialization(e.to_string()))?;

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
        return Err(AmaraError::MalformedPacket("capability payload too short".into()));
    }

    let json_len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    if payload.len() < 2 + json_len + 12 {
        return Err(AmaraError::MalformedPacket("capability payload truncated".into()));
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
        return Err(AmaraError::MalformedPacket("capability hash mismatch".into()));
    }

    Ok((doc, version, hash))
}

/// Encode a task for wire transmission.
pub fn encode_task(task: &Task) -> Result<Vec<u8>> {
    serde_json::to_vec(task)
        .map_err(|e| AmaraError::Serialization(e.to_string()))
}

/// Decode a task from wire format.
pub fn decode_task(payload: &[u8]) -> Result<Task> {
    serde_json::from_slice(payload)
        .map_err(|e| AmaraError::Serialization(format!("invalid task: {e}")))
}

/// Encode a task result for wire transmission.
pub fn encode_result(result: &TaskResult) -> Result<Vec<u8>> {
    serde_json::to_vec(result)
        .map_err(|e| AmaraError::Serialization(e.to_string()))
}

/// Decode a task result from wire format.
pub fn decode_result(payload: &[u8]) -> Result<TaskResult> {
    serde_json::from_slice(payload)
        .map_err(|e| AmaraError::Serialization(format!("invalid result: {e}")))
}

/// Encode an announce payload.
pub fn encode_announce(
    display_name: &str,
    version: &str,
    platform: amara_core::peer::Platform,
    capabilities: &[String],
    load: amara_core::capability::LoadClass,
    availability: amara_core::capability::Availability,
) -> Vec<u8> {
    let name_bytes = display_name.as_bytes();
    let ver_bytes = version.as_bytes();

    let mut payload = Vec::new();

    // Name
    payload.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
    payload.extend_from_slice(name_bytes);

    // Version
    payload.extend_from_slice(&(ver_bytes.len() as u16).to_be_bytes());
    payload.extend_from_slice(ver_bytes);

    // Platform
    payload.push(platform as u8);

    // Capability count + IDs
    payload.push(capabilities.len().min(255) as u8);
    for cap in capabilities.iter().take(255) {
        // Map capability name to category ID for compact wire format
        let cat = match cap.as_str() {
            "reasoning" => amara_core::capability::CapabilityCategory::Reasoning,
            "coding" => amara_core::capability::CapabilityCategory::Coding,
            "vision" => amara_core::capability::CapabilityCategory::Vision,
            "audio" => amara_core::capability::CapabilityCategory::Audio,
            "speech" => amara_core::capability::CapabilityCategory::Speech,
            "ocr" => amara_core::capability::CapabilityCategory::Ocr,
            "search" => amara_core::capability::CapabilityCategory::Search,
            "embeddings" => amara_core::capability::CapabilityCategory::Embeddings,
            "sensors" => amara_core::capability::CapabilityCategory::Sensors,
            _ => amara_core::capability::CapabilityCategory::Custom,
        };
        payload.push(cat.to_id());
    }

    // Load class
    payload.push(load.to_u8());

    // Availability
    payload.push(availability.to_u8());

    payload
}

/// Decode an announce payload.
pub fn decode_announce(
    payload: &[u8],
) -> Result<(
    String,
    String,
    amara_core::peer::Platform,
    Vec<u8>,
    amara_core::capability::LoadClass,
    amara_core::capability::Availability,
)> {
    if payload.len() < 2 {
        return Err(AmaraError::MalformedPacket("announce too short".into()));
    }

    let mut offset = 0;

    // Name
    let name_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
    offset += 2;
    if offset + name_len > payload.len() {
        return Err(AmaraError::MalformedPacket("announce name truncated".into()));
    }
    let name = String::from_utf8_lossy(&payload[offset..offset + name_len]).to_string();
    offset += name_len;

    // Version
    if offset + 2 > payload.len() {
        return Err(AmaraError::MalformedPacket("announce version missing".into()));
    }
    let ver_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
    offset += 2;
    if offset + ver_len > payload.len() {
        return Err(AmaraError::MalformedPacket("announce version truncated".into()));
    }
    let version = String::from_utf8_lossy(&payload[offset..offset + ver_len]).to_string();
    offset += ver_len;

    // Platform + capability count
    if offset + 2 > payload.len() {
        return Err(AmaraError::MalformedPacket("announce metadata truncated".into()));
    }
    let platform = amara_core::peer::Platform::from_u8(payload[offset]);
    offset += 1;

    let cap_count = payload[offset] as usize;
    offset += 1;

    if offset + cap_count > payload.len() {
        return Err(AmaraError::MalformedPacket("announce capabilities truncated".into()));
    }
    let cap_ids: Vec<u8> = payload[offset..offset + cap_count].to_vec();
    offset += cap_count;

    // Load + availability
    let load = if offset < payload.len() {
        let l = amara_core::capability::LoadClass::from_u8(payload[offset]);
        offset += 1;
        l
    } else {
        amara_core::capability::LoadClass::Idle
    };

    let availability = if offset < payload.len() {
        amara_core::capability::Availability::from_u8(payload[offset])
    } else {
        amara_core::capability::Availability::Online
    };

    Ok((name, version, platform, cap_ids, load, availability))
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
        let encoded = encode_announce(
            "nova",
            "0.1.0",
            amara_core::peer::Platform::Linux,
            &["reasoning".to_string(), "coding".to_string()],
            LoadClass::Low,
            Availability::Online,
        );

        let (name, version, platform, caps, load, avail) = decode_announce(&encoded).unwrap();

        assert_eq!(name, "nova");
        assert_eq!(version, "0.1.0");
        assert_eq!(platform, amara_core::peer::Platform::Linux);
        assert_eq!(caps.len(), 2);
        assert_eq!(load, LoadClass::Low);
        assert_eq!(avail, Availability::Online);
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
