//! Fragment/reassembly for large messages.
//!
//! When a message exceeds the transport MTU (default 500 bytes for BLE),
//! the protocol layer splits it into fragments and reassembles at the destination.

use amara_core::{AmaraError, MAX_FRAGMENTS, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Fragment header (12 bytes), appended after the standard packet header.
#[derive(Debug, Clone)]
pub struct FragmentHeader {
    /// Group ID (same as parent message_id, 8 bytes).
    pub group_id: [u8; 8],
    /// Fragment index (0-based).
    pub index: u16,
    /// Total number of fragments.
    pub total: u16,
}

impl FragmentHeader {
    pub const SIZE: usize = 12;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.group_id);
        buf[8..10].copy_from_slice(&self.index.to_be_bytes());
        buf[10..12].copy_from_slice(&self.total.to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::SIZE {
            return Err(AmaraError::FragmentError("header too short".into()));
        }

        let mut group_id = [0u8; 8];
        group_id.copy_from_slice(&buf[0..8]);
        let index = u16::from_be_bytes([buf[8], buf[9]]);
        let total = u16::from_be_bytes([buf[10], buf[11]]);

        if total == 0 {
            return Err(AmaraError::FragmentError("total fragments is 0".into()));
        }
        if total > MAX_FRAGMENTS {
            return Err(AmaraError::FragmentError(format!(
                "too many fragments: {} (max {})",
                total, MAX_FRAGMENTS
            )));
        }
        if index >= total {
            return Err(AmaraError::FragmentIndexOutOfRange { index, total });
        }

        Ok(Self {
            group_id,
            index,
            total,
        })
    }
}

/// A single fragment with its header and payload.
#[derive(Debug, Clone)]
pub struct Fragment {
    pub header: FragmentHeader,
    pub payload: Vec<u8>,
}

/// Split a message into fragments.
pub fn fragment_message(
    group_id: [u8; 8],
    data: &[u8],
    max_fragment_payload: usize,
) -> Result<Vec<Fragment>> {
    if data.is_empty() {
        return Ok(vec![Fragment {
            header: FragmentHeader {
                group_id,
                index: 0,
                total: 1,
            },
            payload: vec![],
        }]);
    }

    let fragment_count = (data.len() + max_fragment_payload - 1) / max_fragment_payload;
    if fragment_count > MAX_FRAGMENTS as usize {
        return Err(AmaraError::PacketTooLarge {
            size: data.len(),
            max: MAX_FRAGMENTS as usize * max_fragment_payload,
        });
    }

    let total = fragment_count as u16;
    let mut fragments = Vec::with_capacity(fragment_count);

    for (i, chunk) in data.chunks(max_fragment_payload).enumerate() {
        fragments.push(Fragment {
            header: FragmentHeader {
                group_id,
                index: i as u16,
                total,
            },
            payload: chunk.to_vec(),
        });
    }

    Ok(fragments)
}

/// Reassembly buffer for incoming fragments.
pub struct Reassembler {
    /// In-progress assemblies keyed by group_id.
    assemblies: HashMap<[u8; 8], Assembly>,
    /// Maximum concurrent reassemblies.
    max_concurrent: usize,
    /// Timeout for incomplete assemblies.
    timeout: Duration,
    /// Bytes currently buffered across all assemblies.
    buffered: usize,
    /// Upper bound on buffered bytes (memory-exhaustion guard).
    max_buffered: usize,
}

/// Default cap on bytes buffered by a [`Reassembler`] (64 MiB).
pub const DEFAULT_MAX_BUFFERED: usize = 64 * 1024 * 1024;

struct Assembly {
    total: u16,
    received: HashMap<u16, Vec<u8>>,
    started_at: Instant,
    bytes: usize,
}

impl Reassembler {
    pub fn new(max_concurrent: usize, timeout: Duration) -> Self {
        Self {
            assemblies: HashMap::new(),
            max_concurrent,
            timeout,
            buffered: 0,
            max_buffered: DEFAULT_MAX_BUFFERED,
        }
    }

    /// Override the buffered-bytes cap.
    pub fn with_byte_limit(mut self, max_buffered: usize) -> Self {
        self.max_buffered = max_buffered;
        self
    }

    /// Add a fragment. Returns the reassembled message if complete.
    pub fn add_fragment(&mut self, fragment: Fragment) -> Result<Option<Vec<u8>>> {
        self.cleanup_expired();

        let group_id = fragment.header.group_id;
        let index = fragment.header.index;
        let total = fragment.header.total;

        if self.buffered + fragment.payload.len() > self.max_buffered {
            return Err(AmaraError::FragmentError("reassembly buffer full".into()));
        }

        // Check if we already have this assembly
        if let Some(assembly) = self.assemblies.get_mut(&group_id) {
            // Verify consistency
            if assembly.total != total {
                return Err(AmaraError::FragmentError("fragment total mismatch".into()));
            }

            // Ignore duplicate fragments
            if assembly.received.contains_key(&index) {
                return Ok(None);
            }

            self.buffered += fragment.payload.len();
            assembly.bytes += fragment.payload.len();
            assembly.received.insert(index, fragment.payload);

            // Check if complete
            if assembly.received.len() == total as usize {
                let assembly = self.assemblies.remove(&group_id).unwrap();
                self.buffered -= assembly.bytes;
                return Ok(Some(assemble(assembly)?));
            }

            return Ok(None);
        }

        // New assembly
        if self.assemblies.len() >= self.max_concurrent {
            return Err(AmaraError::TooManyReassemblies(self.max_concurrent));
        }

        if total == 1 {
            // Single fragment, complete immediately
            return Ok(Some(fragment.payload));
        }

        let bytes = fragment.payload.len();
        let mut received = HashMap::new();
        received.insert(index, fragment.payload);
        self.buffered += bytes;
        self.assemblies.insert(
            group_id,
            Assembly {
                total,
                received,
                started_at: Instant::now(),
                bytes,
            },
        );

        Ok(None)
    }

    /// Remove expired assemblies.
    fn cleanup_expired(&mut self) {
        let timeout = self.timeout;
        let mut freed = 0;
        self.assemblies.retain(|_, a| {
            let keep = a.started_at.elapsed() < timeout;
            if !keep {
                freed += a.bytes;
            }
            keep
        });
        self.buffered -= freed;
    }

    /// Number of in-progress assemblies.
    pub fn pending_count(&self) -> usize {
        self.assemblies.len()
    }
}

/// Assemble fragments in order.
fn assemble(assembly: Assembly) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    for i in 0..assembly.total {
        let fragment_data = assembly
            .received
            .get(&i)
            .ok_or_else(|| AmaraError::FragmentError(format!("missing fragment {}", i)))?;
        data.extend_from_slice(fragment_data);
    }
    Ok(data)
}

/// Codec for converting between packets and fragment-level payloads.
/// This is a thin wrapper used by the mesh engine.
pub mod codec_utils {
    use super::*;

    /// Encode a fragment to bytes (fragment header + payload).
    pub fn encode_fragment(fragment: &Fragment) -> Vec<u8> {
        let header_bytes = fragment.header.encode();
        let mut buf = Vec::with_capacity(FragmentHeader::SIZE + fragment.payload.len());
        buf.extend_from_slice(&header_bytes);
        buf.extend_from_slice(&fragment.payload);
        buf
    }

    /// Decode a fragment from bytes.
    pub fn decode_fragment(data: &[u8]) -> Result<Fragment> {
        let header = FragmentHeader::decode(data)?;
        // FragmentHeader::decode guarantees data.len() >= FragmentHeader::SIZE.
        let payload = data[FragmentHeader::SIZE..].to_vec();
        Ok(Fragment { header, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amara_core::DEFAULT_FRAGMENT_PAYLOAD;

    #[test]
    fn fragment_header_roundtrip() {
        let header = FragmentHeader {
            group_id: [1, 2, 3, 4, 5, 6, 7, 8],
            index: 3,
            total: 10,
        };
        let encoded = header.encode();
        let decoded = FragmentHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.group_id, header.group_id);
        assert_eq!(decoded.index, 3);
        assert_eq!(decoded.total, 10);
    }

    #[test]
    fn fragment_and_reassemble() {
        let group_id = [42u8; 8];
        let original = vec![0u8; 2000]; // Larger than one fragment

        let fragments = fragment_message(group_id, &original, DEFAULT_FRAGMENT_PAYLOAD).unwrap();
        assert!(fragments.len() > 1);

        let mut reassembler = Reassembler::new(64, Duration::from_secs(30));

        let mut result = None;
        for frag in fragments {
            if let Some(data) = reassembler.add_fragment(frag).unwrap() {
                result = Some(data);
            }
        }

        let reassembled = result.expect("should have reassembled");
        assert_eq!(reassembled, original);
    }

    #[test]
    fn duplicate_fragments_ignored() {
        let group_id = [1u8; 8];
        let fragments = fragment_message(group_id, &[0u8; 1000], DEFAULT_FRAGMENT_PAYLOAD).unwrap();
        let mut reassembler = Reassembler::new(64, Duration::from_secs(30));

        // Add first fragment twice
        let frag = fragments[0].clone();
        assert!(reassembler.add_fragment(frag.clone()).unwrap().is_none());
        assert!(reassembler.add_fragment(frag).unwrap().is_none()); // Duplicate, should be ignored

        // Complete the rest
        for frag in fragments.into_iter().skip(1) {
            let _ = reassembler.add_fragment(frag).unwrap();
        }
    }

    #[test]
    fn out_of_order_reassembly() {
        let group_id = [7u8; 8];
        let original = vec![0xAB; 1500];
        let mut fragments =
            fragment_message(group_id, &original, DEFAULT_FRAGMENT_PAYLOAD).unwrap();

        // Reverse order
        fragments.reverse();

        let mut reassembler = Reassembler::new(64, Duration::from_secs(30));
        let mut result = None;
        for frag in fragments {
            if let Some(data) = reassembler.add_fragment(frag).unwrap() {
                result = Some(data);
            }
        }

        assert_eq!(result.unwrap(), original);
    }

    #[test]
    fn too_many_concurrent_assemblies() {
        let mut reassembler = Reassembler::new(2, Duration::from_secs(30));

        // Start two assemblies
        for i in 0..2 {
            let frag = Fragment {
                header: FragmentHeader {
                    group_id: [i; 8],
                    index: 0,
                    total: 3,
                },
                payload: vec![i],
            };
            reassembler.add_fragment(frag).unwrap();
        }

        // Third should fail
        let frag = Fragment {
            header: FragmentHeader {
                group_id: [99; 8],
                index: 0,
                total: 3,
            },
            payload: vec![99],
        };
        let result = reassembler.add_fragment(frag);
        assert!(matches!(result, Err(AmaraError::TooManyReassemblies(_))));
    }

    #[test]
    fn byte_limit_enforced() {
        let mut reassembler = Reassembler::new(64, Duration::from_secs(30)).with_byte_limit(100);
        let frag = |i: u16| Fragment {
            header: FragmentHeader {
                group_id: [3; 8],
                index: i,
                total: 4,
            },
            payload: vec![0; 60],
        };
        assert!(reassembler.add_fragment(frag(0)).unwrap().is_none());
        assert!(reassembler.add_fragment(frag(1)).is_err());
    }

    #[test]
    fn invalid_fragment_index() {
        let buf = {
            let mut b = [0u8; 12];
            b[8..10].copy_from_slice(&5u16.to_be_bytes()); // index = 5
            b[10..12].copy_from_slice(&3u16.to_be_bytes()); // total = 3
            b
        };
        let result = FragmentHeader::decode(&buf);
        assert!(matches!(
            result,
            Err(AmaraError::FragmentIndexOutOfRange { index: 5, total: 3 })
        ));
    }

    #[test]
    fn empty_message_fragment() {
        let group_id = [0u8; 8];
        let fragments = fragment_message(group_id, &[], DEFAULT_FRAGMENT_PAYLOAD).unwrap();
        assert_eq!(fragments.len(), 1);
        assert!(fragments[0].payload.is_empty());
    }
}
