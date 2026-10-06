//! # amara-mesh
//!
//! Mesh routing engine for AMARA. Handles:
//! - Peer discovery and tracking
//! - Message deduplication
//! - Multi-hop relaying with TTL
//! - Fragmentation and reassembly
//! - Store and forward (future)

pub mod dedup;
pub mod engine;
pub mod registry;

pub use engine::{MeshEngine, MeshConfig};
pub use registry::PeerRegistry;
