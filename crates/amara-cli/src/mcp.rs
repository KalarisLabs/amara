//! Model Context Protocol server over stdio (JSON-RPC 2.0, one message per line).
//!
//! Gives any MCP client (Claude Code, Claude Desktop, Codex, Cursor, Windsurf,
//! VS Code, Gemini CLI, …) a mesh identity and seven tools: peers, send,
//! inbox, call, card, expose, reply. stdout carries protocol messages only;
//! logs go to stderr.

use crate::ops;
use amara_agent::Agent;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "AMARA connects you to other AI agents nearby over an encrypted, \
serverless mesh. `peers` lists agents and the tools they offer. `call` runs a tool on another \
agent (omit `to` to auto-route by tool name). `send` messages an agent (or everyone). Check \
`inbox` for messages and for calls to tools you `expose`; answer calls with `reply`.";

pub async fn serve(agent: Agent) -> anyhow::Result<()> {
    let agent = Arc::new(agent);
    let (out_tx, mut out_rx) = mpsc::channel::<Value>(256);

    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = out_rx.recv().await {
            let mut line = serde_json::to_vec(&msg).unwrap_or_default();
            line.push(b'\n');
            if stdout.write_all(&line).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = out_tx
                    .send(error(Value::Null, -32700, &format!("parse error: {e}")))
                    .await;
                continue;
            }
        };
        // Batches are not used by current MCP revisions, but handle them gracefully.
        let messages = match msg {
            Value::Array(items) => items,
            single => vec![single],
        };
        for msg in messages {
            let agent = agent.clone();
            let out = out_tx.clone();
            tokio::spawn(async move {
                if let Some(response) = handle(&agent, msg).await {
                    let _ = out.send(response).await;
                }
            });
        }
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

/// Handle one JSON-RPC message. Notifications (no `id`) produce no response.
pub async fn handle(agent: &Agent, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = match id {
        None | Some(Value::Null) if method.is_empty() => return None, // a response to us; ignore
        None => return None,                                          // notification
        Some(id) => id,
    };

    Some(match method.as_str() {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = SUPPORTED_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .copied()
                .unwrap_or(SUPPORTED_VERSIONS[0]);
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "amara", "title": "AMARA mesh", "version": amara_core::AMARA_VERSION},
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(
            id,
            json!({ "tools": ops::schema("mcp").unwrap_or_default() }),
        ),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !ops::meta_tools().iter().any(|t| t.name == name) {
                return Some(error(id, -32602, &format!("unknown tool: {name}")));
            }
            let (text, is_error) = match ops::run_text(agent, name, &args).await {
                Ok(text) => (text, false),
                Err(e) => (format!("error: {e}"), true),
            };
            result(
                id,
                json!({"content":[{"type":"text","text":text}],"isError":is_error}),
            )
        }
        "resources/list" => result(id, json!({"resources": []})),
        "prompts/list" => result(id, json!({"prompts": []})),
        _ => error(id, -32601, &format!("method not found: {method}")),
    })
}
