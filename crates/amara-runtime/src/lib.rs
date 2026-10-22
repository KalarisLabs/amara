//! # amara-runtime
//!
//! The Agent Runtime layer of AMARA. It sits above the mesh and turns
//! authenticated packets into agent semantics: tools, calls, replies,
//! messages, presence and policy.

pub mod inbox;
pub mod runtime;
pub mod tool;

pub use inbox::{Inbox, InboxItem};
pub use runtime::{AgentRuntime, PeerSummary, RuntimeConfig};
pub use tool::{CallContext, CommandTool, FnTool, Tool, ToolResult, ToolSpec, validate_tool_name};
