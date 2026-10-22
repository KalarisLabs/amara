//! Operations shared by the MCP server, HTTP API, CLI and chat REPL, plus the
//! compact text renderings handed to LLMs (designed to cost few tokens).

use amara_agent::{Agent, Delivery, FileReceipt, InboxItem, PeerSummary, ToolSpec, TopicSample};
use amara_core::DEFAULT_CALL_TIMEOUT_SECS;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Default cap on characters of call output returned to an LLM.
pub const DEFAULT_MAX_CHARS: usize = 20_000;
/// Longest inbox long-poll.
pub const MAX_WAIT_SECS: u64 = 300;

/// One of AMARA's agent-facing tools (exposed via MCP and as function-calling schemas).
pub struct MetaTool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

pub fn meta_tools() -> Vec<MetaTool> {
    vec![
        MetaTool {
            name: "peers",
            description: "List nearby AMARA agents: name, id, hops, load, capabilities/tools.",
            schema: json!({"type":"object","properties":{
                "capability":{"type":"string","description":"Only agents offering this tool or tag"}}}),
        },
        MetaTool {
            name: "send",
            description: "Send a message. `to` = agent name/id for an end-to-end encrypted DM; omit for a public broadcast.",
            schema: json!({"type":"object","required":["text"],"properties":{
                "to":{"type":"string"},
                "text":{"type":"string"},
                "channel":{"type":"string","description":"Broadcast channel, e.g. #lab"}}}),
        },
        MetaTool {
            name: "inbox",
            description: "Read new messages and incoming tool calls (consumes them). `wait` seconds blocks until something arrives.",
            schema: json!({"type":"object","properties":{
                "wait":{"type":"integer","minimum":0,"maximum":MAX_WAIT_SECS},
                "max":{"type":"integer","minimum":1,"maximum":100}}}),
        },
        MetaTool {
            name: "call",
            description: "Call a tool on another agent. `to` = name/id, `@capability`, or omit to auto-pick the best agent offering `tool`. Use `card` first if you need the argument schema. Partial output streams as progress notifications when your client supports them.",
            schema: json!({"type":"object","required":["tool"],"properties":{
                "to":{"type":"string"},
                "tool":{"type":"string"},
                "args":{"type":"object"},
                "timeout":{"type":"integer","description":"Seconds (default 60)"},
                "max_chars":{"type":"integer","description":"Truncate output (default 20000)"}}}),
        },
        MetaTool {
            name: "card",
            description: "Get an agent's tools with full input schemas.",
            schema: json!({"type":"object","required":["to"],"properties":{"to":{"type":"string"}}}),
        },
        MetaTool {
            name: "expose",
            description: "Offer a tool to the mesh that YOU answer: calls appear in `inbox`; respond with `reply`.",
            schema: json!({"type":"object","required":["name","description"],"properties":{
                "name":{"type":"string","pattern":"^[a-z0-9_.-]{1,64}$"},
                "description":{"type":"string"},
                "input_schema":{"type":"object"},
                "remove":{"type":"boolean","description":"Stop offering this tool"}}}),
        },
        MetaTool {
            name: "reply",
            description: "Answer an incoming tool call from the inbox with `result` (any JSON) or `error`. For calls marked streaming, `partial: true` sends `result` as progress and keeps the call open.",
            schema: json!({"type":"object","required":["call_id"],"properties":{
                "call_id":{"type":"string"},
                "result":{},
                "error":{"type":"string"},
                "partial":{"type":"boolean"}}}),
        },
        MetaTool {
            name: "topics",
            description: "Live values robots, sensors and agents publish nearby (newest per sender). `topic` filters: exact, `rover1/*`, or `*`.",
            schema: json!({"type":"object","properties":{
                "topic":{"type":"string"}}}),
        },
        MetaTool {
            name: "publish",
            description: "Publish a value on a topic to everyone within `hops` (default 2), e.g. topic=\"lab/status\".",
            schema: json!({"type":"object","required":["topic"],"properties":{
                "topic":{"type":"string","pattern":"^[a-z0-9_.-]+(/[a-z0-9_.-]+)*$"},
                "data":{},
                "hops":{"type":"integer","minimum":1,"maximum":7}}}),
        },
        MetaTool {
            name: "estop",
            description: "EMERGENCY STOP: tell every robot in range (or only `to`) to halt immediately. Use whenever something looks unsafe.",
            schema: json!({"type":"object","required":["reason"],"properties":{
                "to":{"type":"string","description":"One robot (name/id); omit for all"},
                "reason":{"type":"string"}}}),
        },
        MetaTool {
            name: "send_file",
            description: "Send a local file to an agent in range (end-to-end encrypted, verified by hash). It lands in their inbox.",
            schema: json!({"type":"object","required":["to","path"],"properties":{
                "to":{"type":"string"},
                "path":{"type":"string","description":"Absolute path on this machine"}}}),
        },
    ]
}

/// Tool definitions in a function-calling format: `mcp`, `openai` or `anthropic`.
pub fn schema(format: &str) -> Result<Value> {
    let tools = meta_tools();
    Ok(match format {
        "mcp" => json!(tools.iter().map(|t| json!({"name":t.name,"description":t.description,"inputSchema":t.schema})).collect::<Vec<_>>()),
        "openai" => json!(tools.iter().map(|t| json!({"type":"function","function":{
            "name":format!("amara_{}", t.name),"description":t.description,"parameters":t.schema}})).collect::<Vec<_>>()),
        "anthropic" => json!(tools.iter().map(|t| json!({
            "name":format!("amara_{}", t.name),"description":t.description,"input_schema":t.schema})).collect::<Vec<_>>()),
        other => bail!("unknown schema format '{other}' (use mcp, openai or anthropic)"),
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

// ── renderers ────────────────────────────────────────────────────────────────

pub fn render_peers(agent: &Agent, peers: &[PeerSummary]) -> String {
    let me = agent.identity();
    let tools: Vec<String> = agent.tools().into_iter().map(|t| t.name).collect();
    let mut out = format!(
        "me: {} {}{}\n",
        me.display_name(),
        short(&me.peer_id().to_string()),
        if tools.is_empty() {
            String::new()
        } else {
            format!(" · serving: {}", tools.join(", "))
        }
    );
    if peers.is_empty() {
        out.push_str("no agents nearby yet (discovery takes a few seconds)");
        return out;
    }
    out.push_str(&format!("{} agent(s) nearby:\n", peers.len()));
    let width = peers
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(4)
        .min(24);
    for p in peers {
        out.push_str(&format!(
            "{:<width$} {} {} hop{} {:?}{}\n",
            p.name,
            short(&p.id),
            p.hops,
            if p.hops == 1 { " " } else { "s" },
            p.load,
            if p.capabilities.is_empty() {
                String::new()
            } else {
                format!("  {}", p.capabilities.join(", "))
            },
            width = width
        ));
    }
    out.trim_end().to_string()
}

pub fn render_inbox(items: &[InboxItem]) -> String {
    if items.is_empty() {
        return "inbox empty".into();
    }
    items
        .iter()
        .map(|item| match item {
            InboxItem::Message { from, from_name, text, channel, direct, stored, .. } => {
                let scope = match (direct, channel) {
                    (true, _) => "dm".to_string(),
                    (false, Some(c)) => c.clone(),
                    (false, None) => "public".to_string(),
                };
                let late = if *stored { " delivered-late" } else { "" };
                format!("[msg {scope}{late}] {from_name} ({}): {text}", short(from))
            }
            InboxItem::Call { call_id, from, from_name, tool, args, expires_in, stream, .. } => format!(
                "[call{}] {from_name} ({}) → {tool} {} · reply(call_id=\"{call_id}\") within {expires_in}s",
                if *stream { " streaming" } else { "" },
                short(from),
                compact(args, 2000)
            ),
            InboxItem::File { from, from_name, name, path, size, .. } => format!(
                "[file] {from_name} ({}) sent {name} ({}) → {path}",
                short(from),
                human_size(*size)
            ),
            InboxItem::Stop { from, from_name, reason, .. } => format!(
                "[EMERGENCY STOP] {from_name} ({}): {reason}",
                short(from)
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compact JSON (strings unquoted) truncated to `max` chars.
pub fn compact(value: &Value, max: usize) -> String {
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    truncate(text, max)
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

pub fn truncate(text: String, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…[truncated {} chars]", count - max)
}

// ── operations ───────────────────────────────────────────────────────────────

pub fn peers(agent: &Agent, capability: Option<&str>) -> Vec<PeerSummary> {
    let mut peers = agent.peers();
    if let Some(cap) = capability {
        peers.retain(|p| p.capabilities.iter().any(|c| c == cap));
    }
    peers
}

/// Returns a short confirmation.
pub async fn send(
    agent: &Agent,
    to: Option<&str>,
    text: &str,
    channel: Option<&str>,
) -> Result<String> {
    if text.is_empty() {
        bail!("text is empty");
    }
    match to.filter(|t| *t != "*" && !t.is_empty()) {
        Some(target) => {
            let (id, name) = agent.resolve_any(target)?;
            let id_text = id.to_string();
            Ok(match agent.send_message(id, text).await? {
                Delivery::Sent => format!("sent encrypted DM to {name} ({})", short(&id_text)),
                Delivery::Stored => format!(
                    "{name} ({}) is out of range; DM stored and will be delivered when they are back (by you or any nearby agent)",
                    short(&id_text)
                ),
            })
        }
        None => {
            agent
                .broadcast_message(text, channel.map(str::to_string))
                .await?;
            Ok(format!(
                "broadcast to {} nearby agent(s){}",
                agent.peers().len(),
                channel.map(|c| format!(" on {c}")).unwrap_or_default()
            ))
        }
    }
}

pub struct CallOutcome {
    pub ok: bool,
    pub data: Value,
    pub ms: u64,
    pub peer_name: String,
    pub peer_id: String,
}

pub async fn call(
    agent: &Agent,
    to: Option<&str>,
    tool: &str,
    args: Value,
    timeout_secs: Option<u64>,
) -> Result<CallOutcome> {
    call_streaming(agent, to, tool, args, timeout_secs, None).await
}

/// Like [`call`], forwarding partial output to `chunks` when given.
pub async fn call_streaming(
    agent: &Agent,
    to: Option<&str>,
    tool: &str,
    args: Value,
    timeout_secs: Option<u64>,
    chunks: Option<mpsc::UnboundedSender<Value>>,
) -> Result<CallOutcome> {
    let peer = agent.resolve_target(to, tool)?;
    let timeout = Duration::from_secs(
        timeout_secs
            .unwrap_or(DEFAULT_CALL_TIMEOUT_SECS)
            .clamp(1, 3600),
    );
    let args = if args.is_null() { json!({}) } else { args };
    let reply = match chunks {
        Some(tx) => {
            agent
                .call_streaming(peer.peer_id, tool, args, Some(timeout), tx)
                .await?
        }
        None => agent.call(peer.peer_id, tool, args, Some(timeout)).await?,
    };
    Ok(CallOutcome {
        ok: reply.ok,
        data: reply.data,
        ms: reply.ms,
        peer_name: peer.display_name,
        peer_id: peer.peer_id.to_string(),
    })
}

pub async fn card(agent: &Agent, to: &str) -> Result<Value> {
    let out = call(agent, Some(to), "amara.card", json!({}), Some(15)).await?;
    if !out.ok {
        bail!("{}", compact(&out.data, 500));
    }
    Ok(out.data)
}

pub fn render_card(card: &Value) -> String {
    let mut out = format!(
        "{} {} ({}){}\n",
        card["name"].as_str().unwrap_or("?"),
        short(card["id"].as_str().unwrap_or("")),
        card["platform"].as_str().unwrap_or("?"),
        card["tags"]
            .as_array()
            .filter(|t| !t.is_empty())
            .map(|t| format!(" tags: {}", compact(&json!(t), 200)))
            .unwrap_or_default()
    );
    match card["tools"].as_array() {
        Some(tools) if !tools.is_empty() => {
            for t in tools {
                out.push_str(&format!(
                    "- {}: {} args={}\n",
                    t["name"].as_str().unwrap_or("?"),
                    t["description"].as_str().unwrap_or(""),
                    serde_json::to_string(&t["input_schema"]).unwrap_or_default()
                ));
            }
        }
        _ => out.push_str("no tools\n"),
    }
    out.trim_end().to_string()
}

pub fn expose(agent: &Agent, args: &Value) -> Result<String> {
    let name = str_arg(args, "name").ok_or_else(|| anyhow!("name is required"))?;
    if args.get("remove").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(if agent.remove_tool(name) {
            format!("stopped offering {name}")
        } else {
            format!("{name} was not offered")
        });
    }
    let mut spec = ToolSpec::new(name, str_arg(args, "description").unwrap_or_default());
    if let Some(schema) = args.get("input_schema").filter(|s| s.is_object()) {
        spec = spec.with_schema(schema.clone());
    }
    agent.expose(spec)?;
    Ok(format!(
        "now offering '{name}' to nearby agents; calls arrive in inbox"
    ))
}

pub fn reply(agent: &Agent, args: &Value) -> Result<String> {
    let id = str_arg(args, "call_id").ok_or_else(|| anyhow!("call_id is required"))?;
    let call_id = Uuid::parse_str(id).map_err(|_| anyhow!("call_id is not a valid id"))?;
    if args
        .get("partial")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        agent.reply_partial(call_id, args.get("result").cloned().unwrap_or(Value::Null))?;
        return Ok("partial result sent; the call is still open".into());
    }
    let outcome = match str_arg(args, "error") {
        Some(err) => Err(err.to_string()),
        None => Ok(args.get("result").cloned().unwrap_or(Value::Null)),
    };
    agent.reply(call_id, outcome)?;
    Ok("reply sent".into())
}

pub async fn inbox(agent: &Agent, wait_secs: u64, max: usize) -> Vec<InboxItem> {
    agent
        .inbox()
        .take(
            max.clamp(1, 100),
            Duration::from_secs(wait_secs.min(MAX_WAIT_SECS)),
        )
        .await
}

pub fn render_topics(samples: &[TopicSample]) -> String {
    if samples.is_empty() {
        return "no topics yet".into();
    }
    samples
        .iter()
        .map(|s| {
            format!(
                "{} · {} ({}) #{} {}s ago: {}",
                s.topic,
                s.from_name,
                short(&s.from),
                s.seq,
                s.age_ms / 1000,
                compact(&s.data, 500)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn publish(agent: &Agent, topic: &str, data: Value, hops: Option<u8>) -> Result<String> {
    let seq = agent.publish(topic, data, hops).await?;
    Ok(format!("published {topic} #{seq}"))
}

pub async fn estop(agent: &Agent, to: Option<&str>, reason: &str) -> Result<String> {
    let target = match to {
        Some(t) => Some(agent.resolve(t)?),
        None => None,
    };
    agent
        .estop(target.as_ref().map(|p| p.peer_id), reason)
        .await?;
    Ok(match target {
        Some(p) => format!("emergency stop sent to {}", p.display_name),
        None => format!(
            "emergency stop broadcast to every robot in range ({} agent(s))",
            agent.peers().len()
        ),
    })
}

/// Send a file from this machine to a peer in range.
pub async fn send_file(agent: &Agent, to: &str, path: &str) -> Result<FileReceipt> {
    let peer = agent.resolve(to)?;
    let path = std::path::Path::new(path);
    Ok(agent.send_file(peer.peer_id, path).await?)
}

pub fn render_receipt(to: &str, r: &FileReceipt) -> String {
    format!(
        "sent {} ({}) to {to} in {}ms, verified{}",
        r.name,
        human_size(r.size),
        r.ms,
        if r.resent > 0 {
            format!(" ({} chunk(s) resent)", r.resent)
        } else {
            String::new()
        }
    )
}

/// Execute a meta tool by name with JSON arguments, returning LLM-friendly text.
/// A `call` forwards partial output to `progress` when given.
pub async fn run_text_streaming(
    agent: &Agent,
    name: &str,
    args: &Value,
    progress: Option<mpsc::UnboundedSender<Value>>,
) -> Result<String> {
    match name {
        "peers" => Ok(render_peers(
            agent,
            &peers(agent, str_arg(args, "capability")),
        )),
        "send" => {
            send(
                agent,
                str_arg(args, "to"),
                str_arg(args, "text").unwrap_or_default(),
                str_arg(args, "channel"),
            )
            .await
        }
        "inbox" => {
            let wait = args.get("wait").and_then(Value::as_u64).unwrap_or(0);
            let max = args.get("max").and_then(Value::as_u64).unwrap_or(20) as usize;
            Ok(render_inbox(&inbox(agent, wait, max).await))
        }
        "call" => {
            let tool = str_arg(args, "tool").ok_or_else(|| anyhow!("tool is required"))?;
            let max = args
                .get("max_chars")
                .and_then(Value::as_u64)
                .map(|m| m as usize)
                .unwrap_or(DEFAULT_MAX_CHARS);
            let out = call_streaming(
                agent,
                str_arg(args, "to"),
                tool,
                args.get("args").cloned().unwrap_or(Value::Null),
                args.get("timeout").and_then(Value::as_u64),
                progress,
            )
            .await?;
            let body = compact(&out.data, max);
            if out.ok {
                Ok(format!(
                    "{} ({}, {}ms): {body}",
                    out.peer_name,
                    short(&out.peer_id),
                    out.ms
                ))
            } else {
                bail!("{} returned error: {body}", out.peer_name)
            }
        }
        "card" => Ok(render_card(
            &card(
                agent,
                str_arg(args, "to").ok_or_else(|| anyhow!("to is required"))?,
            )
            .await?,
        )),
        "expose" => expose(agent, args),
        "reply" => reply(agent, args),
        "topics" => Ok(render_topics(&agent.topics(str_arg(args, "topic")))),
        "publish" => {
            let topic = str_arg(args, "topic").ok_or_else(|| anyhow!("topic is required"))?;
            let hops = args
                .get("hops")
                .and_then(Value::as_u64)
                .map(|h| h.min(7) as u8);
            publish(
                agent,
                topic,
                args.get("data").cloned().unwrap_or(Value::Null),
                hops,
            )
            .await
        }
        "estop" => {
            estop(
                agent,
                str_arg(args, "to"),
                str_arg(args, "reason").unwrap_or("emergency stop"),
            )
            .await
        }
        "send_file" => {
            let to = str_arg(args, "to").ok_or_else(|| anyhow!("to is required"))?;
            let path = str_arg(args, "path").ok_or_else(|| anyhow!("path is required"))?;
            Ok(render_receipt(to, &send_file(agent, to, path).await?))
        }
        other => bail!("unknown tool '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_in_all_formats() {
        for f in ["mcp", "openai", "anthropic"] {
            let s = schema(f).unwrap();
            assert_eq!(s.as_array().unwrap().len(), meta_tools().len());
        }
        assert!(schema("xml").is_err());
        assert_eq!(
            schema("openai").unwrap()[0]["function"]["name"],
            "amara_peers"
        );
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate("héllo".into(), 10), "héllo");
        assert_eq!(truncate("héllo wörld".into(), 3), "hél…[truncated 8 chars]");
        assert_eq!(compact(&json!("plain"), 100), "plain");
        assert_eq!(compact(&json!({"a":1}), 100), r#"{"a":1}"#);
    }

    #[test]
    fn inbox_rendering() {
        let id = Uuid::nil();
        let text = render_inbox(&[
            InboxItem::Message {
                from: "0123456789abcdef".into(),
                from_name: "nova".into(),
                text: "hi".into(),
                channel: None,
                direct: true,
                at: 0,
                stored: true,
            },
            InboxItem::Call {
                call_id: id,
                from: "fedcba9876543210".into(),
                from_name: "orion".into(),
                tool: "review".into(),
                args: json!({"x":1}),
                at: 0,
                expires_in: 300,
                stream: false,
            },
            InboxItem::File {
                from: "fedcba9876543210".into(),
                from_name: "orion".into(),
                name: "a.png".into(),
                path: "/tmp/a.png".into(),
                size: 2048,
                hash: String::new(),
                at: 0,
            },
        ]);
        assert!(text.contains("[msg dm delivered-late] nova (01234567): hi"));
        assert!(text.contains("[file] orion (fedcba98) sent a.png (2.0 KB) → /tmp/a.png"));
        assert!(text.contains(&format!("reply(call_id=\"{id}\")")));
        assert_eq!(render_inbox(&[]), "inbox empty");
    }
}
