//! # amara-runtime
//!
//! The Agent Runtime layer of AMARA. It sits above the mesh and turns
//! authenticated packets into agent semantics: tools, calls (including
//! streaming calls), replies, messages, file transfers, live topics, emergency
//! stops, presence and policy.

pub mod files;
pub mod inbox;
pub mod runtime;
pub mod tool;
pub mod topics;

pub use amara_mesh::Delivery;
pub use files::{AcceptFiles, FILE_CHUNK_SIZE, FilePolicy, FileReceipt, sanitize_file_name};
pub use inbox::{Inbox, InboxItem};
pub use runtime::{AgentRuntime, PeerSummary, RuntimeConfig};
pub use tool::{
    CallContext, CommandTool, FnTool, StreamSink, Tool, ToolResult, ToolSpec, validate_tool_name,
};
pub use topics::{
    DEFAULT_TOPIC_HOPS, ESTOP_TOPIC, EstopFrom, EstopPolicy, TopicSample, topic_matches,
    validate_topic,
};
