//! Task types for the AMARA agent protocol.

use crate::peer::PeerId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique task identifier (UUID v4).
pub type TaskId = Uuid;

/// Task priority level (0=lowest, 10=highest, 5=normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskPriority(pub u8);

impl TaskPriority {
    pub const LOW: Self = Self(2);
    pub const NORMAL: Self = Self(5);
    pub const HIGH: Self = Self(8);
    pub const CRITICAL: Self = Self(10);

    pub fn new(priority: u8) -> Self {
        Self(priority.min(10))
    }
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Current status of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Task created, not yet sent.
    Created,
    /// Task sent, awaiting acceptance.
    Pending,
    /// Task accepted by target agent.
    Accepted,
    /// Task is being executed.
    Running,
    /// Task completed successfully.
    Completed,
    /// Task failed.
    Failed,
    /// Task was rejected by target.
    Rejected,
    /// Task was cancelled.
    Cancelled,
    /// Task expired before completion.
    Expired,
    /// Task is queued at the target.
    Queued,
}

/// An AMARA task — the fundamental unit of work in the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task identifier.
    pub task_id: TaskId,
    /// Parent task ID (for delegation chains). None if this is a root task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskId>,
    /// PeerId of the task issuer.
    pub issuer: PeerId,
    /// Target PeerId (None for capability-based routing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PeerId>,
    /// Required capability name.
    pub capability: String,
    /// Task priority.
    #[serde(default)]
    pub priority: TaskPriority,
    /// Deadline as Unix timestamp (0 = no deadline).
    #[serde(default)]
    pub deadline: u32,
    /// Input data (JSON-serializable).
    pub input: serde_json::Value,
    /// Constraints on execution.
    #[serde(default)]
    pub constraints: TaskConstraints,
    /// Current status.
    #[serde(default = "default_status")]
    pub status: TaskStatus,
    /// Delegation depth counter.
    #[serde(default)]
    pub delegation_depth: u8,
    /// Set of agent PeerIds that have already processed this task (loop prevention).
    #[serde(default)]
    pub visited_agents: Vec<PeerId>,
}

fn default_status() -> TaskStatus {
    TaskStatus::Created
}

/// Constraints on task execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskConstraints {
    /// Maximum cost (0 = free).
    #[serde(default)]
    pub max_cost: u32,
    /// Maximum latency in milliseconds (0 = no limit).
    #[serde(default)]
    pub max_latency_ms: u32,
    /// Maximum delegation depth.
    #[serde(default = "default_max_depth")]
    pub max_delegation_depth: u8,
}

fn default_max_depth() -> u8 {
    5
}

/// Result of a completed task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Task ID this result corresponds to.
    pub task_id: TaskId,
    /// Status of the result.
    pub status: TaskResultStatus,
    /// Result data (success) or error message (failure).
    pub data: serde_json::Value,
    /// Agent that produced the result.
    pub agent: PeerId,
    /// Execution time in milliseconds.
    #[serde(default)]
    pub execution_time_ms: u64,
}

/// Task result status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum TaskResultStatus {
    Success = 0,
    Error = 1,
    Partial = 2,
    Timeout = 3,
}

impl TaskResultStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Success,
            1 => Self::Error,
            2 => Self::Partial,
            3 => Self::Timeout,
            _ => Self::Error,
        }
    }
}

impl Task {
    /// Create a new task with a generated UUID.
    pub fn new(issuer: PeerId, capability: impl Into<String>, input: serde_json::Value) -> Self {
        Self {
            task_id: Uuid::new_v4(),
            parent_task_id: None,
            issuer,
            target: None,
            capability: capability.into(),
            priority: TaskPriority::default(),
            deadline: 0,
            input,
            constraints: TaskConstraints::default(),
            status: TaskStatus::Created,
            delegation_depth: 0,
            visited_agents: vec![issuer],
        }
    }

    /// Check if delegation depth would be exceeded.
    pub fn can_delegate(&self) -> bool {
        self.delegation_depth < self.constraints.max_delegation_depth
    }

    /// Check if this task has already visited a particular agent (loop detection).
    pub fn has_visited(&self, peer: &PeerId) -> bool {
        self.visited_agents.contains(peer)
    }

    /// Record that an agent has processed this task.
    pub fn record_visit(&mut self, peer: PeerId) {
        if !self.visited_agents.contains(&peer) {
            self.visited_agents.push(peer);
        }
        self.delegation_depth += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_creation() {
        let issuer = PeerId::from_bytes([1; 8]);
        let task = Task::new(issuer, "vision", serde_json::json!({"prompt": "describe image"}));
        assert_eq!(task.capability, "vision");
        assert_eq!(task.status, TaskStatus::Created);
        assert!(task.has_visited(&issuer));
    }

    #[test]
    fn delegation_loop_detection() {
        let a = PeerId::from_bytes([1; 8]);
        let b = PeerId::from_bytes([2; 8]);
        let c = PeerId::from_bytes([3; 8]);

        let mut task = Task::new(a, "reasoning", serde_json::json!({}));
        assert!(task.can_delegate());

        task.record_visit(b);
        assert!(task.has_visited(&b));
        assert!(!task.has_visited(&c));

        task.record_visit(c);
        assert_eq!(task.delegation_depth, 2);
    }

    #[test]
    fn max_delegation_depth() {
        let a = PeerId::from_bytes([1; 8]);
        let mut task = Task::new(a, "reasoning", serde_json::json!({}));
        task.constraints.max_delegation_depth = 2;

        task.record_visit(PeerId::from_bytes([2; 8]));
        assert!(task.can_delegate());
        task.record_visit(PeerId::from_bytes([3; 8]));
        assert!(!task.can_delegate());
    }
}
