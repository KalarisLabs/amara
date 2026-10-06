use amara_runtime::{AgentRuntime, TaskHandler};
use std::sync::Arc;
use tracing::info;

/// High-level Agent builder/manager.
pub struct Agent {
    name: String,
    runtime: Arc<AgentRuntime>,
}

impl Agent {
    /// Create a new Agent attached to a runtime.
    pub fn new(name: impl Into<String>, runtime: Arc<AgentRuntime>) -> Self {
        let name = name.into();
        info!("Creating agent: {}", name);
        Self { name, runtime }
    }

    /// Register a capability handler.
    pub fn register_capability(&self, handler: Box<dyn TaskHandler>) {
        self.runtime.register_handler(handler);
    }
}
