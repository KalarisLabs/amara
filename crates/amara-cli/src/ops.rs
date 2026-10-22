//! Operations shared by the MCP server, HTTP API, CLI and chat REPL, plus the
//! compact text renderings handed to LLMs (designed to cost few tokens).

use amara_agent::{Agent, InboxItem, PeerSummary, ToolSpec};
use amara_core::DEFAULT_CALL_TIMEOUT_SECS;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;
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
            description: "Call a tool on another agent. `to` = name/id, `@capability`, or omit to auto-pick the best agent offering `tool`. Use `card` first if you need the argument schema.",
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
            description: "Answer an incoming tool call from the inbox with `result` (any JSON) or `error`.",
            schema: json!({"type":"object","required":["call_id"],"properties":{
                "call_id":{"type":"string"},
                "result":{},
                "error":{"type":"string"}}}),
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
            InboxItem::Message { from, from_name, text, channel, direct, .. } => {
                let scope = match (direct, channel) {
                    (true, _) => "dm".to_string(),
                    (false, Some(c)) => c.clone(),
                    (false, None) => "public".to_string(),
                };
                format!("[msg {scope}] {from_name} ({}): {text}", short(from))
            }
            InboxItem::Call { call_id, from, from_name, tool, args, expires_in, .. } => format!(
                "[call] {from_name} ({}) → {tool} {} · reply(call_id=\"{call_id}\") within {expires_in}s",
                short(from),
                compact(args, 2000)
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
            let peer = agent.resolve(target)?;
            agent.send_message(peer.peer_id, text).await?;
            Ok(format!(
                "sent encrypted DM to {} ({})",
                peer.display_name,
                short(&peer.peer_id.to_string())
            ))
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
    let peer = agent.resolve_target(to, tool)?;
    let timeout = Duration::from_secs(
        timeout_secs
            .unwrap_or(DEFAULT_CALL_TIMEOUT_SECS)
            .clamp(1, 3600),
    );
    let args = if args.is_null() { json!({}) } else { args };
    let reply = agent.call(peer.peer_id, tool, args, Some(timeout)).await?;
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

/// Execute a meta tool by name with JSON arguments, returning LLM-friendly text.
pub async fn run_text(agent: &Agent, name: &str, args: &Value) -> Result<String> {
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
            let out = call(
                agent,
                str_arg(args, "to"),
                tool,
                args.get("args").cloned().unwrap_or(Value::Null),
                args.get("timeout").and_then(Value::as_u64),
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
            },
            InboxItem::Call {
                call_id: id,
                from: "fedcba9876543210".into(),
                from_name: "orion".into(),
                tool: "review".into(),
                args: json!({"x":1}),
                at: 0,
                expires_in: 300,
            },
        ]);
        assert!(text.contains("[msg dm] nova (01234567): hi"));
        assert!(text.contains(&format!("reply(call_id=\"{id}\")")));
        assert_eq!(render_inbox(&[]), "inbox empty");
    }
}
