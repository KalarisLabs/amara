//! Application message bodies carried inside packets.
//!
//! | Packet type | Body      | Purpose                                  |
//! |-------------|-----------|------------------------------------------|
//! | `TASK`      | [`Call`]  | Invoke a tool on a remote agent          |
//! | `RESULT`    | [`Reply`] | Result (or error) of a [`Call`]          |
//! | `MESSAGE`   | [`Chat`]  | Free-form text, direct or on a channel   |
//! | `RESULT_CHUNK` | [`Chunk`] | Partial output of a streaming call    |
//! | `EVENT`     | [`Sample`] | A value published on a topic           |
//! | `FILE_MANIFEST` | [`FileOffer`] | Announces a file transfer        |
//! | `FILE_CHUNK` | [`FileChunk`] | One slice of a file (binary body)      |
//! | `FILE_COMPLETE` | [`FileStatus`] | Transfer progress, both directions |
//!
//! Bodies are JSON (so any language can speak AMARA) and are LZ4-compressed
//! when that makes them smaller. Field names are deliberately short. The one
//! exception is [`FileChunk`], which is binary so files do not pay base64.

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
    /// The caller accepts partial output as `RESULT_CHUNK` packets before the [`Reply`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream: bool,
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
    /// Number of [`Chunk`]s sent before this reply (streaming calls only).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chunks: u32,
}

/// Partial output of a streaming [`Call`]. `seq` starts at 0; chunks may arrive
/// out of order and the caller re-orders them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: Uuid,
    pub seq: u32,
    #[serde(default)]
    pub data: Value,
}

/// One value published on a topic (`EVENT`, broadcast, signed, not sealed).
/// Topics are how robots and sensors share live state: `rover1/battery`,
/// `lab/temperature`, `cam2/detections`. `seq` increases per (sender, topic),
/// so receivers keep only the newest value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub topic: String,
    pub seq: u64,
    #[serde(default)]
    pub data: Value,
}

/// Offer of a file (`FILE_MANIFEST`). The receiver answers with a [`FileStatus`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileOffer {
    pub id: Uuid,
    pub name: String,
    pub size: u64,
    /// Bytes per [`FileChunk`] (the last one may be shorter).
    pub chunk: u32,
    /// Hex BLAKE3 of the whole file.
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

impl FileOffer {
    /// Number of chunks the file is split into.
    pub fn chunks(&self) -> u32 {
        if self.chunk == 0 {
            return 0;
        }
        self.size.div_ceil(self.chunk as u64).min(u32::MAX as u64) as u32
    }
}

/// Where a transfer stands (`FILE_COMPLETE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    /// Receiver → sender: go ahead.
    Accepted,
    /// Receiver → sender: refused (see `error`).
    Rejected,
    /// Sender → receiver: every chunk has been sent once; report what is missing.
    Sent,
    /// Receiver → sender: resend the chunks in `missing`.
    Missing,
    /// Receiver → sender: file complete and hash verified.
    Received,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileStatus {
    pub id: Uuid,
    pub state: FileState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One slice of a file: `id (16) · index u32 · data`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileChunk {
    pub id: Uuid,
    pub index: u32,
    pub data: Vec<u8>,
}

impl FileChunk {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20 + self.data.len());
        buf.extend_from_slice(self.id.as_bytes());
        buf.extend_from_slice(&self.index.to_be_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 20 {
            return Err(AmaraError::MalformedPacket("file chunk too short".into()));
        }
        let id = Uuid::from_slice(&bytes[..16])
            .map_err(|e| AmaraError::MalformedPacket(e.to_string()))?;
        let index = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        Ok(Self {
            id,
            index,
            data: bytes[20..].to_vec(),
        })
    }
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

fn is_false(v: &bool) -> bool {
    !*v
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
                stream: false,
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
    fn streaming_fields_are_optional_on_the_wire() {
        let reply: Reply =
            serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000000","ok":true}"#)
                .unwrap();
        assert_eq!(reply.chunks, 0);
        let call = Call {
            id: Uuid::nil(),
            tool: "t".into(),
            args: json!({}),
            timeout_ms: 0,
            stream: true,
        };
        let text = serde_json::to_string(&call).unwrap();
        assert!(text.contains("\"stream\":true"));
        assert_eq!(serde_json::from_str::<Call>(&text).unwrap(), call);
    }

    #[test]
    fn file_chunk_and_offer() {
        let chunk = FileChunk {
            id: Uuid::new_v4(),
            index: 7,
            data: vec![1, 2, 3],
        };
        assert_eq!(FileChunk::decode(&chunk.encode()).unwrap(), chunk);
        assert!(FileChunk::decode(&[0; 19]).is_err());
        let offer = FileOffer {
            id: Uuid::nil(),
            name: "a".into(),
            size: 65,
            chunk: 32,
            hash: String::new(),
            mime: None,
        };
        assert_eq!(offer.chunks(), 3);
        assert_eq!(FileOffer { size: 0, ..offer }.chunks(), 0);
    }

    #[test]
    fn decompression_bomb_rejected() {
        let mut evil = (u32::MAX).to_le_bytes().to_vec();
        evil.extend_from_slice(&[0; 8]);
        assert!(decode_body::<Chat>(&evil, true).is_err());
    }
}
