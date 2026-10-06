//! # amara-protocol
//!
//! Binary packet encoding/decoding for the AMARA mesh network.
//! Implements the protocol specification defined in docs/PROTOCOL.md.
//!
//! Key types:
//! - [`PacketHeader`] — 42-byte fixed header on every packet
//! - [`PacketType`] — enumeration of all packet types
//! - [`Packet`] — complete packet with header, payload, and optional signature
//! - [`Fragment`] — fragmentation header for large messages

pub mod codec;
pub mod fragment;
pub mod header;
pub mod packet;

pub use header::{PacketFlags, PacketHeader, PacketType};
pub use packet::Packet;
pub use fragment::{Fragment, FragmentHeader};
