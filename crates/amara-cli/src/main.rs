//! `amara` — the BitChat for AI agents.

mod api;
mod chat;
mod client;
mod config;
mod mcp;
mod ops;
mod setup;
mod simulate;
mod style;

use crate::config::{
    DaemonInfo, NodeArgs, api_bind, api_token, daemon_path, start_node, write_private,
};
use crate::style::{bold, dim, green, red, yellow};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "amara",
    version,
    about = "AMARA — the BitChat for AI agents: serverless, end-to-end encrypted mesh with tool calling",
    after_help = "Run `amara` with no command to open the interactive chat."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    node: NodeArgs,
    /// More logs (repeat for more).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Command {
    /// Run a node with the local HTTP API (for apps, scripts and the CLI commands below).
    Up {
        /// HTTP API bind address.
        #[arg(long)]
        api: Option<String>,
    },
    /// Interactive chat on the mesh (default command).
    Chat,
    /// Model Context Protocol server over stdio (for Claude, Codex, Cursor, …).
    Mcp,
    /// Connect an AI app to the mesh via MCP.
    Setup {
        #[arg(value_enum)]
        client: setup::Client,
        /// Apply the change instead of printing it.
        #[arg(long)]
        write: bool,
    },
    /// List nearby agents (needs `amara up`).
    Peers {
        #[arg(long)]
        json: bool,
    },
    /// Send a message: `amara send nova "hi"`, or `amara send "*" "hello all"`.
    Send { to: String, text: String },
    /// Call a tool: `amara call nova summarize '{"text":"..."}'` (`-` = auto-pick agent).
    Call {
        to: String,
        tool: String,
        args: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Show an agent's tools and schemas.
    Card { to: String },
    /// Read (and consume) the inbox.
    Inbox {
        /// Seconds to wait for the first item.
        #[arg(long, default_value_t = 0)]
        wait: u64,
        #[arg(long)]
        json: bool,
    },
    /// Print function-calling tool definitions for any LLM: mcp | openai | anthropic.
    Schema {
        #[arg(default_value = "openai")]
        format: String,
    },
    /// In-process three-agent demo (multi-hop, E2E, capability routing).
    Simulate,
    /// Check that this machine can join the mesh.
    Doctor,
    /// Show this agent's identity.
    Id,
}

fn init_logging(verbose: u8, quiet_default: bool) {
    let default = match (verbose, quiet_default) {
        (0, true) => "warn",
        (0, false) => "info,amara=info",
        (1, _) => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let quiet = !matches!(cli.command, Some(Command::Up { .. }));
    init_logging(cli.verbose, quiet);
    if let Err(e) = run(cli).await {
        eprintln!("{} {e:#}", red("error:"));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command.unwrap_or(Command::Chat) {
        Command::Up { api } => up(&cli.node, api.as_deref()).await,
        Command::Chat => {
            let node = start_node(&cli.node, None).await?;
            chat::run(node.agent.clone()).await
        }
        Command::Mcp => {
            let node = start_node(&cli.node, Some("mcp")).await?;
            mcp::serve(node.agent.clone()).await
        }
        Command::Setup { client, write } => setup::run(client, cli.node.name.clone(), write),
        Command::Peers { json } => {
            let peers = client::Client::discover()?.get("/v1/peers")?;
            if json {
                println!("{}", serde_json::to_string_pretty(&peers)?);
            } else {
                print_peers(&peers);
            }
            Ok(())
        }
        Command::Send { to, text } => {
            let res =
                client::Client::discover()?.post("/v1/send", &json!({"to": to, "text": text}))?;
            println!("{}", res["status"].as_str().unwrap_or("sent"));
            Ok(())
        }
        Command::Call {
            to,
            tool,
            args,
            timeout,
        } => {
            let args: Value = match args {
                Some(raw) => serde_json::from_str(&raw).context("args must be JSON")?,
                None => json!({}),
            };
            let to = (to != "-").then_some(to);
            let res = client::Client::discover()?.post(
                "/v1/call",
                &json!({"to": to, "tool": tool, "args": args, "timeout_secs": timeout}),
            )?;
            println!("{}", serde_json::to_string_pretty(&res["data"])?);
            if res["ok"] != json!(true) {
                std::process::exit(2);
            }
            Ok(())
        }
        Command::Card { to } => {
            let card = client::Client::discover()?
                .get(&format!("/v1/card/{}", client::encode_segment(&to)))?;
            println!("{}", ops::render_card(&card));
            Ok(())
        }
        Command::Inbox { wait, json } => {
            let items = client::Client::discover()?.get(&format!("/v1/inbox?wait={wait}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                print_inbox(&items);
            }
            Ok(())
        }
        Command::Schema { format } => {
            println!("{}", serde_json::to_string_pretty(&ops::schema(&format)?)?);
            Ok(())
        }
        Command::Simulate => simulate::run().await,
        Command::Doctor => doctor(&cli.node, None).await,
        Command::Id => {
            let node = start_node(&cli.node, None).await?;
            let id = node.agent.identity();
            println!(
                "{} {}\n{} {}",
                bold("name"),
                id.display_name(),
                bold("id  "),
                id.peer_id()
            );
            println!("{} {}", bold("key "), hex::encode(id.public_key_bytes()));
            println!(
                "{} {}",
                bold("file"),
                amara_agent::identity_path(id.display_name()).display()
            );
            Ok(())
        }
    }
}

fn print_peers(peers: &Value) {
    let list = peers.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("{}", dim("no agents nearby yet"));
        return;
    }
    let width = list
        .iter()
        .filter_map(|p| p["name"].as_str())
        .map(str::len)
        .max()
        .unwrap_or(4)
        .min(24);
    for p in list {
        let caps: Vec<&str> = p["capabilities"]
            .as_array()
            .map(|c| c.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let id = p["id"].as_str().unwrap_or("");
        let hops = p["hops"].as_u64().unwrap_or(0);
        println!(
            "{} {:<width$} {} {} {:<4}  {}",
            green("●"),
            p["name"].as_str().unwrap_or("?"),
            dim(&id[..8.min(id.len())]),
            hops,
            if hops == 1 { "hop" } else { "hops" },
            caps.join(", "),
            width = width
        );
    }
}

fn print_inbox(items: &Value) {
    let list = items.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("{}", dim("inbox empty"));
    }
    for item in list {
        match item["kind"].as_str() {
            Some("message") => println!(
                "{} {}: {}",
                dim(if item["direct"] == json!(true) {
                    "[dm]"
                } else {
                    item["channel"].as_str().unwrap_or("[all]")
                }),
                bold(item["from_name"].as_str().unwrap_or("?")),
                item["text"].as_str().unwrap_or("")
            ),
            _ => println!(
                "{} {} → {} {}  {}",
                yellow("[call]"),
                item["from_name"].as_str().unwrap_or("?"),
                item["tool"].as_str().unwrap_or("?"),
                ops::compact(&item["args"], 500),
                dim(&format!(
                    "call_id={}",
                    item["call_id"].as_str().unwrap_or("")
                ))
            ),
        }
    }
}

async fn up(args: &NodeArgs, api: Option<&str>) -> Result<()> {
    let bind = api_bind(api)?;
    let token = api_token()?;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| {
            format!("cannot bind HTTP API on {bind} (another `amara up` running? use --api)")
        })?;
    let node = start_node(args, None).await?;
    let agent = Arc::new(node.agent.clone());
    let id = agent.identity();
    let addr = listener.local_addr()?;
    let info = DaemonInfo {
        url: format!("http://{addr}"),
        name: id.display_name().to_string(),
        id: id.peer_id().to_string(),
        pid: std::process::id(),
    };
    write_private(&daemon_path(), &serde_json::to_string_pretty(&info)?)?;

    eprintln!(
        "\n  {} {} {}\n  {} http://{addr}  {}\n  {} {}\n",
        bold("AMARA"),
        bold(id.display_name()),
        dim(&id.peer_id().to_string()),
        dim("api "),
        dim("(Authorization: Bearer <~/.amara/api.token>)"),
        dim("mesh"),
        dim("LAN multicast discovery on; Ctrl-C to stop")
    );

    let app = api::router(agent.clone(), token);
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    let result = server.await;
    let _ = std::fs::remove_file(daemon_path());
    agent.shutdown();
    drop(node);
    result.context("HTTP API failed")
}

async fn doctor(args: &NodeArgs, api: Option<&str>) -> Result<()> {
    let mut ok = true;
    let mut check = |name: &str, result: Result<String>| match result {
        Ok(detail) => println!("{} {name} {}", green("✓"), dim(&detail)),
        Err(e) => {
            ok = false;
            println!("{} {name}: {e:#}", red("✗"));
        }
    };

    let home = amara_agent::amara_home();
    check(
        "data directory",
        (|| {
            std::fs::create_dir_all(&home)?;
            let probe = home.join(".probe");
            std::fs::write(&probe, b"ok")?;
            std::fs::remove_file(probe)?;
            Ok(home.display().to_string())
        })(),
    );
    check(
        "config",
        config::Config::load().map(|_| config::Config::path().display().to_string()),
    );

    let port = args.port.unwrap_or(amara_core::DEFAULT_MESH_PORT);
    check(
        "UDP unicast socket",
        std::net::UdpSocket::bind("0.0.0.0:0")
            .map(|s| {
                format!(
                    "bound {}",
                    s.local_addr().map(|a| a.to_string()).unwrap_or_default()
                )
            })
            .map_err(Into::into),
    );
    check(
        "LAN multicast",
        (|| {
            let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
            socket.join_multicast_v4(
                &amara_core::MESH_MULTICAST_V4.into(),
                &std::net::Ipv4Addr::UNSPECIFIED,
            )?;
            Ok(format!(
                "group 239.77.65.82:{port} (allow UDP {port} in your firewall)"
            ))
        })(),
    );

    let bind = api_bind(api)?;
    let daemon = client::Client::discover().and_then(|c| c.get("/v1/status"));
    match daemon {
        Ok(status) => println!(
            "{} node running {} {}",
            green("✓"),
            bold(status["name"].as_str().unwrap_or("?")),
            dim(&format!("{} peer(s)", status["peers"]))
        ),
        Err(_) => check(
            "HTTP API port",
            std::net::TcpListener::bind(&bind)
                .map(|_| format!("{bind} free"))
                .map_err(|e| anyhow::anyhow!("{bind}: {e}")),
        ),
    }

    println!();
    if ok {
        println!(
            "{} ready. Try `amara simulate`, `amara`, or `amara setup claude-code`.",
            green("●")
        );
        Ok(())
    } else {
        anyhow::bail!("some checks failed")
    }
}
