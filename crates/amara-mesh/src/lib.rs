//! # amara-mesh
//!
//! Mesh routing engine for AMARA. Handles:
//! - Peer discovery and tracking
//! - Message deduplication
//! - Multi-hop relaying with TTL
//! - Signature verification against self-certifying announces
//! - Learned next-hop routes for unicast traffic
//! - Per-hop fragmentation and reassembly
//! - Store and forward for peers that are out of range

pub mod dedup;
pub mod engine;
pub mod registry;
pub mod store;

pub use engine::{Delivery, Inbound, MeshConfig, MeshEngine};
pub use registry::PeerRegistry;
pub use store::KnownPeer;
