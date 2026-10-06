//! # amara-transport
//!
//! Transport layer abstraction for the AMARA network.
//!
//! Defines the `Transport` trait and provides implementations:
//! - `SimulatedTransport`: For deterministic mesh simulation
//! - `LoopbackTransport`: For local-only communication

pub mod loopback;
pub mod simulated;
pub mod traits;

pub use traits::{Transport, TransportEvent};
pub use loopback::LoopbackTransport;
pub use simulated::{SimulatedTransport, SimulatorNode, LinkConfig};
