//! # amara-core
//!
//! Core types, errors, and constants shared across all AMARA crates.
//! This crate has no knowledge of networking, cryptography internals,
//! or agent runtime specifics. It defines the vocabulary of the system.

pub mod capability;
pub mod constants;
pub mod error;
pub mod peer;
pub mod task;
pub mod types;

pub use capability::{Availability, Capability, CapabilityCategory, CapabilityDocument, LoadClass};
pub use constants::*;
pub use error::{AmaraError, Result};
pub use peer::{PeerId, PeerInfo, Platform, TrustLevel};
pub use task::{Task, TaskId, TaskPriority, TaskStatus};
pub use types::*;
