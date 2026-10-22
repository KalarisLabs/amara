//! Link-layer framing: splits encoded packets into transport-sized frames.
//!
//! ```text
//! frame = 0xA7 kind body
//! kind 0x00  body = complete packet
//! kind 0x01  body = FragmentHeader (12) || chunk
//! ```
//!
//! Framing happens per hop, below signatures, so relays forward whole packets
//! and each link can use its own MTU (BLE ~500 B, UDP 1400 B).

use crate::fragment::{Reassembler, codec_utils, fragment_message};
use amara_core::{
    AmaraError, FRAGMENT_REASSEMBLY_TIMEOUT_SECS, MAX_CONCURRENT_REASSEMBLIES, Result,
};
use std::time::Duration;

const MAGIC: u8 = 0xA7;
const KIND_WHOLE: u8 = 0x00;
const KIND_FRAGMENT: u8 = 0x01;
const FRAME_OVERHEAD: usize = 2;

/// Split an encoded packet into frames no larger than `mtu` bytes.
pub fn encode_frames(packet: &[u8], mtu: usize) -> Result<Vec<Vec<u8>>> {
    if packet.len() + FRAME_OVERHEAD <= mtu {
        let mut frame = Vec::with_capacity(packet.len() + FRAME_OVERHEAD);
        frame.extend_from_slice(&[MAGIC, KIND_WHOLE]);
        frame.extend_from_slice(packet);
        return Ok(vec![frame]);
    }
    let chunk = mtu
        .checked_sub(FRAME_OVERHEAD + crate::FragmentHeader::SIZE)
        .filter(|c| *c > 0)
        .ok_or_else(|| AmaraError::Transport(format!("mtu {mtu} too small")))?;
    let group = amara_core::types::random_message_id();
    Ok(fragment_message(group, packet, chunk)?
        .iter()
        .map(|f| {
            let mut frame = vec![MAGIC, KIND_FRAGMENT];
            frame.extend_from_slice(&codec_utils::encode_fragment(f));
            frame
        })
        .collect())
}

/// Reassembles frames from one or more links back into packets.
pub struct FrameAssembler {
    reassembler: Reassembler,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self {
            reassembler: Reassembler::new(
                MAX_CONCURRENT_REASSEMBLIES,
                Duration::from_secs(FRAGMENT_REASSEMBLY_TIMEOUT_SECS),
            ),
        }
    }
}

impl FrameAssembler {
    /// Feed one frame. Returns a complete packet when one is ready.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>> {
        match frame {
            [MAGIC, KIND_WHOLE, rest @ ..] => Ok(Some(rest.to_vec())),
            [MAGIC, KIND_FRAGMENT, rest @ ..] => self
                .reassembler
                .add_fragment(codec_utils::decode_fragment(rest)?),
            _ => Err(AmaraError::MalformedPacket("not an AMARA frame".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_packet_single_frame() {
        let frames = encode_frames(b"hello", 100).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(
            FrameAssembler::default().push(&frames[0]).unwrap().unwrap(),
            b"hello"
        );
    }

    #[test]
    fn large_packet_fragments_and_reassembles_out_of_order() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let mut frames = encode_frames(&data, 500).unwrap();
        assert!(frames.len() > 40 && frames.iter().all(|f| f.len() <= 500));
        frames.reverse();
        let mut asm = FrameAssembler::default();
        let out: Vec<_> = frames.iter().filter_map(|f| asm.push(f).unwrap()).collect();
        assert_eq!(out, vec![data]);
    }

    #[test]
    fn garbage_rejected() {
        assert!(FrameAssembler::default().push(&[1, 2, 3]).is_err());
        assert!(
            FrameAssembler::default()
                .push(&[MAGIC, KIND_FRAGMENT, 1])
                .is_err()
        );
    }
}
