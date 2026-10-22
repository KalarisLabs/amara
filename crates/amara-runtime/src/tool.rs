//! Tools: the unit of capability an agent exposes to the mesh.
//!
//! Three kinds of tools exist:
//! * [`FnTool`] — a Rust async closure (SDK users).
//! * [`CommandTool`] — a local program; JSON args on stdin, result on stdout.
//! * Inbox tools — served by an external AI (Claude, Codex, …) that reads the
//!   call from the inbox and answers with `reply` (see `AgentRuntime::expose`).

use amara_core::peer::PeerId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Public description of a tool (what peers see in an agent card).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the arguments object.
    #[serde(default = "default_schema")]
    pub input_schema: Value,
}

fn default_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: default_schema(),
        }
    }

    pub fn with_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }
}

/// Validate a user-defined tool name: `[a-z0-9_.-]{1,64}`, not in the reserved `amara.` namespace.
pub fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("tool name must be 1-64 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
    {
        return Err("tool name may only contain a-z 0-9 _ . -".into());
    }
    if name.starts_with("amara.") {
        return Err("the amara. prefix is reserved for built-in tools".into());
    }
    Ok(())
}

/// Everything a tool learns about an invocation.
#[derive(Debug, Clone)]
pub struct CallContext {
    pub call_id: Uuid,
    pub caller: PeerId,
    pub caller_name: String,
    pub args: Value,
}

/// Result type returned by tools: JSON on success, message on failure.
pub type ToolResult = Result<Value, String>;
type BoxFuture = Pin<Box<dyn Future<Output = ToolResult> + Send>>;

/// A locally executed tool.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, ctx: CallContext) -> ToolResult;
}

/// A tool backed by an async closure.
#[derive(Clone)]
pub struct FnTool {
    spec: ToolSpec,
    f: Arc<dyn Fn(CallContext) -> BoxFuture + Send + Sync>,
}

impl FnTool {
    pub fn new<F, Fut>(spec: ToolSpec, f: F) -> Self
    where
        F: Fn(CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        Self {
            spec,
            f: Arc::new(move |ctx| Box::pin(f(ctx))),
        }
    }
}

#[async_trait::async_trait]
impl Tool for FnTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn call(&self, ctx: CallContext) -> ToolResult {
        (self.f)(ctx).await
    }
}

/// Maximum stdout accepted from a command tool (1 MiB).
const MAX_COMMAND_OUTPUT: usize = 1024 * 1024;

/// A tool that runs a local program. No shell is involved: `program` is
/// executed directly with fixed `args`, the call's JSON arguments are written
/// to stdin, and stdout becomes the result (parsed as JSON when possible).
#[derive(Debug, Clone)]
pub struct CommandTool {
    spec: ToolSpec,
    program: String,
    args: Vec<String>,
    timeout: Duration,
}

impl CommandTool {
    pub fn new(
        spec: ToolSpec,
        program: impl Into<String>,
        args: Vec<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            spec,
            program: program.into(),
            args,
            timeout,
        }
    }
}

#[async_trait::async_trait]
impl Tool for CommandTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn call(&self, ctx: CallContext) -> ToolResult {
        let mut child = tokio::process::Command::new(&self.program)
            .args(&self.args)
            .env("AMARA_CALLER", ctx.caller.to_string())
            .env("AMARA_CALLER_NAME", &ctx.caller_name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("failed to start {}: {e}", self.program))?;

        if let Some(mut stdin) = child.stdin.take() {
            let input = serde_json::to_vec(&ctx.args).unwrap_or_default();
            // A program that ignores stdin closes it early; that is not an error.
            let _ = stdin.write_all(&input).await;
        }

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                format!(
                    "{} timed out after {}s",
                    self.spec.name,
                    self.timeout.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr
                .chars()
                .rev()
                .take(500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(format!("exit {}: {}", output.status, tail.trim()));
        }
        let stdout = &output.stdout[..output.stdout.len().min(MAX_COMMAND_OUTPUT)];
        Ok(serde_json::from_slice(stdout).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(stdout).trim_end().to_string())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_rules() {
        assert!(validate_tool_name("summarize").is_ok());
        assert!(validate_tool_name("ocr.v2-fast_1").is_ok());
        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name("Bad Name").is_err());
        assert!(validate_tool_name("amara.card").is_err());
        assert!(validate_tool_name(&"x".repeat(65)).is_err());
    }

    fn ctx(args: Value) -> CallContext {
        CallContext {
            call_id: Uuid::new_v4(),
            caller: PeerId::from_bytes([1; 8]),
            caller_name: "t".into(),
            args,
        }
    }

    #[tokio::test]
    async fn fn_tool_runs() {
        let tool = FnTool::new(ToolSpec::new("double", "x*2"), |c| async move {
            let x = c.args["x"].as_i64().ok_or("x missing")?;
            Ok(serde_json::json!(x * 2))
        });
        assert_eq!(
            tool.call(ctx(serde_json::json!({"x": 21}))).await.unwrap(),
            42
        );
        assert!(tool.call(ctx(serde_json::json!({}))).await.is_err());
    }

    #[tokio::test]
    async fn command_tool_missing_program_errors() {
        let tool = CommandTool::new(
            ToolSpec::new("nope", ""),
            "amara-no-such-program",
            vec![],
            Duration::from_secs(5),
        );
        assert!(
            tool.call(ctx(Value::Null))
                .await
                .unwrap_err()
                .contains("failed to start")
        );
    }
}
