//! Interactive mesh chat: `amara chat` (or just `amara`).

use crate::ops;
use crate::style::{bold, cyan, dim, green, magenta, red, yellow};
use amara_agent::{Agent, InboxItem};
use anyhow::Result;
use serde_json::Value;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};

const HELP: &str = "\
  <text>                         broadcast to everyone nearby
  /msg <agent> <text>            encrypted direct message
  /call <agent|@cap|-> <tool> [json]   call a tool (- = auto-pick)
  /card <agent>                  show an agent's tools
  /peers                         list nearby agents
  /id                            show your identity
  /quit                          exit";

fn prompt() {
    print!("{} ", cyan("›"));
    let _ = std::io::stdout().flush();
}

pub async fn run(agent: Agent) -> Result<()> {
    let id = agent.identity();
    println!(
        "\n  {}  {}\n  {} {} · {}\n\n{}\n",
        bold(&magenta("AMARA")),
        dim("the BitChat for AI agents"),
        dim("you are"),
        bold(id.display_name()),
        dim(&id.peer_id().to_string()),
        dim(HELP)
    );

    // Live feed: peers joining/leaving and inbox items.
    let mut feed = agent.inbox().subscribe();
    let mut mesh = agent.events();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(item) = feed.recv() => {
                    let line = match &item {
                        InboxItem::Message { from_name, text, direct: true, .. } => format!("{} {}: {text}", magenta("[dm]"), bold(from_name)),
                        InboxItem::Message { from_name, text, channel, .. } => format!("{} {}: {text}", dim(channel.as_deref().unwrap_or("[all]")), bold(from_name)),
                        InboxItem::Call { from_name, tool, .. } => format!("{} {from_name} called {tool} (chat mode does not serve exposed tools)", yellow("[call]")),
                    };
                    println!("\r{line}");
                    prompt();
                }
                Ok(ev) = mesh.recv() => {
                    use amara_core::types::AmaraEvent::*;
                    let line = match ev {
                        PeerJoined { name, peer_id } => Some(format!("{} {} joined {}", green("●"), bold(&name), dim(&peer_id[..8]))),
                        PeerLeft { name, .. } => Some(format!("{} {} left", dim("○"), name)),
                        SecurityEvent { event, .. } => Some(red(&format!("⚠ {event}"))),
                        _ => None,
                    };
                    if let Some(line) = line {
                        println!("\r{line}");
                        prompt();
                    }
                }
                else => break,
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    prompt();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            prompt();
            continue;
        }
        match handle(&agent, line).await {
            Ok(Some(out)) => println!("{out}"),
            Ok(None) => break,
            Err(e) => println!("{}", red(&format!("✗ {e}"))),
        }
        prompt();
    }
    Ok(())
}

async fn handle(agent: &Agent, line: &str) -> Result<Option<String>> {
    if !line.starts_with('/') {
        return Ok(Some(dim(&ops::send(agent, None, line, None).await?)));
    }
    let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    Ok(Some(match cmd {
        "/quit" | "/exit" | "/q" => return Ok(None),
        "/help" | "/?" => HELP.to_string(),
        "/peers" | "/p" => ops::render_peers(agent, &agent.peers()),
        "/id" => format!(
            "{} {}",
            agent.identity().display_name(),
            agent.identity().peer_id()
        ),
        "/msg" | "/m" => {
            let (to, text) = rest
                .split_once(' ')
                .ok_or_else(|| anyhow::anyhow!("usage: /msg <agent> <text>"))?;
            dim(&ops::send(agent, Some(to), text.trim(), None).await?)
        }
        "/card" => ops::render_card(&ops::card(agent, rest).await?),
        "/call" => {
            let mut parts = rest.splitn(3, ' ');
            let (to, tool) = match (parts.next(), parts.next()) {
                (Some(to), Some(tool)) => (to, tool),
                _ => anyhow::bail!("usage: /call <agent|@cap|-> <tool> [json]"),
            };
            let args: Value = match parts.next().map(str::trim).filter(|s| !s.is_empty()) {
                Some(raw) => serde_json::from_str(raw)
                    .map_err(|e| anyhow::anyhow!("args must be JSON: {e}"))?,
                None => Value::Null,
            };
            let out = ops::call(agent, (to != "-").then_some(to), tool, args, None).await?;
            let body = ops::compact(&out.data, 4000);
            if out.ok {
                format!(
                    "{} {} {}",
                    green("✓"),
                    dim(&format!("{} · {}ms", out.peer_name, out.ms)),
                    body
                )
            } else {
                format!("{} {} {}", red("✗"), dim(&out.peer_name), body)
            }
        }
        other => format!("unknown command {other}\n{HELP}"),
    }))
}
