//! Application message bodies carried inside packets.
//!
//! | Packet type | Body      | Purpose                                  |
//! |-------------|-----------|------------------------------------------|
//! | `TASK`      | [`Call`]  | Invoke a tool on a remote agent          |
//! | `RESULT`    | [`Reply`] | Result (or error) of a [`Call`]          |
//! | `MESSAGE`   | [`Chat`]  | Free-form text, direct or on a channel   |
//!
//! Bodies are JSON (so any language can speak AMARA) and are LZ4-compressed
//! when that makes them smaller. Field names are deliberately short.

use amara_core::{AmaraError, COMPRESSION_THRESHOLD, MAX_PACKET_SIZE, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Remote tool invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// Correlates the [`Reply`].
    pub id: Uuid,
    /// Tool name, e.g. `summarize` or the built-in `amara.card`.
    pub tool: String,
    /// JSON arguments for the tool.
    #[serde(default)]
    pub args: Value,
    /// How long the caller will wait, so the callee can give up early (0 = default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub timeout_ms: u32,
}

/// Result of a [`Call`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: Uuid,
    /// `true` for success; on failure `data` is `{"error": "..."}`.
    pub ok: bool,
    #[serde(default)]
    pub data: Value,
    /// Execution time in milliseconds.
    #[serde(default)]
    pub ms: u64,
}

/// Text message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub text: String,
    /// Optional channel for broadcast messages (e.g. `#lab`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Serialize a body. Returns the bytes and whether they are LZ4-compressed.
pub fn encode_body<T: Serialize>(body: &T) -> Result<(Vec<u8>, bool)> {
    let json = serde_json::to_vec(body).map_err(|e| AmaraError::Serialization(e.to_string()))?;
    if json.len() > COMPRESSION_THRESHOLD {
        let compressed = lz4_flex::compress_prepend_size(&json);
        if compressed.len() < json.len() {
            return Ok((compressed, true));
        }
    }
    Ok((json, false))
}

/// Deserialize a body produced by [`encode_body`].
pub fn decode_body<T: DeserializeOwned>(bytes: &[u8], compressed: bool) -> Result<T> {
    let owned;
    let json = if compressed {
        // Refuse decompression bombs before allocating.
        let claimed = bytes
            .get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| AmaraError::MalformedPacket("compressed body too short".into()))?;
        if claimed > MAX_PACKET_SIZE {
            return Err(AmaraError::PacketTooLarge {
                size: claimed,
                max: MAX_PACKET_SIZE,
            });
        }
        owned = lz4_flex::decompress_size_prepended(bytes)
            .map_err(|e| AmaraError::MalformedPacket(format!("lz4: {e}")))?;
        &owned[..]
    } else {
        bytes
    };
    serde_json::from_slice(json).map_err(|e| AmaraError::Serialization(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn call_roundtrip_small_and_large() {
        for args in [
            json!({"q": "hi"}),
            json!({"text": "lorem ipsum ".repeat(200)}),
        ] {
            let call = Call {
                id: Uuid::new_v4(),
                tool: "summarize".into(),
                args,
                timeout_ms: 0,
            };
            let (bytes, compressed) = encode_body(&call).unwrap();
            assert_eq!(decode_body::<Call>(&bytes, compressed).unwrap(), call);
        }
    }

    #[test]
    fn large_bodies_compress() {
        let chat = Chat {
            text: "agent ".repeat(500),
            channel: None,
        };
        let (bytes, compressed) = encode_body(&chat).unwrap();
        assert!(compressed && bytes.len() < 500);
    }

    #[test]
    fn decompression_bomb_rejected() {
        let mut evil = (u32::MAX).to_le_bytes().to_vec();
        evil.extend_from_slice(&[0; 8]);
        assert!(decode_body::<Chat>(&evil, true).is_err());
    }
}
