//! # amara-transport
//!
//! Transport layer abstraction for the AMARA network.
//!
//! Defines the `Transport` trait and provides implementations:
//! - `SimulatedTransport`: For deterministic mesh simulation
//! - `LoopbackTransport`: For local-only communication
//! - `UdpTransport`: Zero-config LAN mesh (IPv4 multicast discovery + unicast)

pub mod loopback;
pub mod simulated;
pub mod traits;
#[cfg(feature = "udp")]
pub mod udp;

pub use loopback::LoopbackTransport;
pub use simulated::{LinkConfig, SimulatedTransport, SimulatorHub, SimulatorNode};
pub use traits::{Transport, TransportEvent};
#[cfg(feature = "udp")]
pub use udp::{UdpConfig, UdpTransport};
