//! # amara-mesh
//!
//! Mesh routing engine for AMARA. Handles:
//! - Peer discovery and tracking
//! - Message deduplication
//! - Multi-hop relaying with TTL
//! - Signature verification against self-certifying announces
//! - Learned next-hop routes for unicast traffic
//! - Per-hop fragmentation and reassembly

pub mod dedup;
pub mod engine;
pub mod registry;

pub use engine::{Inbound, MeshConfig, MeshEngine};
pub use registry::PeerRegistry;
