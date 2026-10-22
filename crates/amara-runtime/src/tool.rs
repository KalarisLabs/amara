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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
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

/// Where a tool sends partial output while it runs. Inactive unless the caller
/// asked for a streaming call, in which case every [`StreamSink::send`] reaches
/// the caller as a `RESULT_CHUNK` before the final result.
#[derive(Clone, Default)]
pub struct StreamSink(Option<mpsc::UnboundedSender<Value>>);

impl StreamSink {
    pub fn new(tx: mpsc::UnboundedSender<Value>) -> Self {
        Self(Some(tx))
    }

    /// True when the caller is listening for partial output.
    pub fn is_active(&self) -> bool {
        self.0.as_ref().is_some_and(|tx| !tx.is_closed())
    }

    /// Send a piece of partial output. Returns false when nobody is listening.
    pub fn send(&self, data: Value) -> bool {
        self.0.as_ref().is_some_and(|tx| tx.send(data).is_ok())
    }
}

impl std::fmt::Debug for StreamSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.is_active() {
            "StreamSink(active)"
        } else {
            "StreamSink(inactive)"
        })
    }
}

/// Everything a tool learns about an invocation.
#[derive(Debug, Clone)]
pub struct CallContext {
    pub call_id: Uuid,
    pub caller: PeerId,
    pub caller_name: String,
    pub args: Value,
    /// Partial output channel (streaming calls only).
    pub stream: StreamSink,
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
/// For streaming calls, each stdout line is also sent as it is printed.
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

        let mut stdout_pipe = child.stdout.take().ok_or("no stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr")?;
        let stream = ctx.stream;
        let stdout_task = tokio::spawn(async move {
            let mut out = Vec::new();
            if stream.is_active() {
                let mut lines = BufReader::new(stdout_pipe);
                let mut line = Vec::new();
                while lines.read_until(b'\n', &mut line).await.unwrap_or(0) > 0 {
                    if out.len() < MAX_COMMAND_OUTPUT {
                        out.extend_from_slice(&line);
                    }
                    let text = String::from_utf8_lossy(&line).trim_end().to_string();
                    stream.send(Value::String(text));
                    line.clear();
                }
            } else {
                let _ = (&mut stdout_pipe)
                    .take(MAX_COMMAND_OUTPUT as u64)
                    .read_to_end(&mut out)
                    .await;
                // Drain the rest so the program never blocks on a full pipe.
                let _ = tokio::io::copy(&mut stdout_pipe, &mut tokio::io::sink()).await;
            }
            out
        });
        let stderr_task = tokio::spawn(async move {
            let mut err = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut err).await;
            err
        });

        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(status) => status.map_err(|e| e.to_string())?,
            Err(_) => {
                let _ = child.kill().await;
                stdout_task.abort();
                stderr_task.abort();
                return Err(format!(
                    "{} timed out after {}s",
                    self.spec.name,
                    self.timeout.as_secs()
                ));
            }
        };
        let output = std::process::Output {
            status,
            stdout: stdout_task.await.unwrap_or_default(),
            stderr: stderr_task.await.unwrap_or_default(),
        };

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
            stream: StreamSink::default(),
        }
    }

    #[tokio::test]
    async fn stream_sink_reports_listeners() {
        assert!(!StreamSink::default().is_active());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = StreamSink::new(tx);
        assert!(sink.is_active() && sink.send(serde_json::json!(1)));
        assert_eq!(rx.recv().await.unwrap(), 1);
        drop(rx);
        assert!(!sink.send(serde_json::json!(2)));
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
