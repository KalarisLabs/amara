use amara_core::task::{Task, TaskResult, TaskResultStatus};
use amara_core::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use crate::runtime::AgentRuntime;

/// Context provided to a task handler when executing a task.
pub struct TaskContext {
    pub task: Task,
    pub runtime: Arc<AgentRuntime>,
}

impl TaskContext {
    /// Helper to create a success result.
    pub fn success(&self, data: Value) -> TaskResult {
        TaskResult {
            task_id: self.task.task_id,
            status: TaskResultStatus::Success,
            data,
            agent: self.runtime.identity().peer_id(),
            execution_time_ms: 0, // Filled in by runtime
        }
    }

    /// Helper to create an error result.
    pub fn error(&self, message: impl Into<String>) -> TaskResult {
        TaskResult {
            task_id: self.task.task_id,
            status: TaskResultStatus::Error,
            data: serde_json::json!({ "error": message.into() }),
            agent: self.runtime.identity().peer_id(),
            execution_time_ms: 0,
        }
    }
}

/// Trait for executing agent tasks.
#[async_trait]
pub trait TaskHandler: Send + Sync {
    /// Return the capability name this handler implements.
    fn capability(&self) -> &str;

    /// Handle a task request.
    async fn handle(&self, ctx: TaskContext) -> Result<TaskResult>;
}
