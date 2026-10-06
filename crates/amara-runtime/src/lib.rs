//! # amara-runtime
//!
//! The Agent Runtime layer of AMARA. This crate sits above the mesh network
//! and manages agent lifecycles, capabilities, task delegation, and execution.

pub mod runtime;
pub mod handler;

pub use runtime::AgentRuntime;
pub use handler::{TaskHandler, TaskContext};
