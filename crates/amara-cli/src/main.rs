//! `amara` — the BitChat for AI agents.

mod api;
mod bench;
mod chat;
mod client;
mod config;
mod mcp;
mod ops;
mod setup;
mod simulate;
mod style;
#[cfg(feature = "tray")]
mod tray;

use crate::config::{
    DaemonInfo, NodeArgs, api_bind, api_token, daemon_path, start_node, write_private,
};
use crate::style::{bold, cyan, dim, green, red, yellow};
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
        /// Serve the API and dashboard to your LAN (for phones; see `amara pair`).
        #[arg(long)]
        lan: bool,
    },
    /// Desktop tray app: a node, the HTTP API and the dashboard, from the menu bar.
    #[cfg(feature = "tray")]
    Tray,
    /// Show a QR code that opens the dashboard on your phone (needs `amara up --lan`).
    Pair,
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
        /// Print partial output as the tool produces it.
        #[arg(long)]
        stream: bool,
    },
    /// Live topic values nearby: `amara topics 'rover1/*' --watch`.
    Topics {
        /// Exact topic, `prefix/*`, or `*` (default).
        pattern: Option<String>,
        /// Keep printing samples as they arrive.
        #[arg(long)]
        watch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Publish a value: `amara publish lab/door '"open"' --hops 3`.
    Publish {
        topic: String,
        data: String,
        #[arg(long)]
        hops: Option<u8>,
    },
    /// EMERGENCY STOP every robot in range (or one: `amara stop arm-1`).
    Stop {
        robot: Option<String>,
        #[arg(long, default_value = "emergency stop")]
        reason: String,
    },
    /// Send a file to an agent in range: `amara send-file nova ./report.pdf`.
    SendFile {
        to: String,
        path: std::path::PathBuf,
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
    /// In-process demo: agents and a robot (calls, streaming, topics, offline delivery, files, e-stop).
    Simulate,
    /// Measure latency and throughput of the full stack on this machine.
    Bench,
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

fn main() {
    let cli = Cli::parse();
    let quiet = !matches!(cli.command, Some(Command::Up { .. }));
    init_logging(cli.verbose, quiet);
    // The tray's event loop must own the main thread (macOS requires it).
    #[cfg(feature = "tray")]
    if matches!(cli.command, Some(Command::Tray)) {
        if let Err(e) = tray::run(cli.node) {
            eprintln!("{} {e:#}", red("error:"));
            std::process::exit(1);
        }
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = runtime.block_on(run(cli)) {
        eprintln!("{} {e:#}", red("error:"));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command.unwrap_or(Command::Chat) {
        Command::Up { api, lan } => {
            let api = match (api, lan) {
                (Some(api), _) => Some(api),
                (None, true) => Some(format!("0.0.0.0:{}", amara_core::DEFAULT_API_PORT)),
                (None, false) => None,
            };
            up(&cli.node, api.as_deref()).await
        }
        #[cfg(feature = "tray")]
        Command::Tray => unreachable!("handled in main"),
        Command::Pair => pair(),
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
            stream,
        } => {
            let args: Value = match args {
                Some(raw) => serde_json::from_str(&raw).context("args must be JSON")?,
                None => json!({}),
            };
            let to = (to != "-").then_some(to);
            let body = json!({"to": to, "tool": tool, "args": args, "timeout_secs": timeout, "stream": stream});
            let res = if stream {
                let mut last = Value::Null;
                client::Client::discover()?.post_events("/v1/call", &body, |event, data| {
                    match event {
                        "chunk" => println!("{}", ops::compact(data, usize::MAX)),
                        _ => last = data.clone(),
                    }
                })?;
                if let Some(err) = last.get("error").and_then(Value::as_str)
                    && last.get("data").is_none()
                {
                    anyhow::bail!("{err}");
                }
                last
            } else {
                client::Client::discover()?.post("/v1/call", &body)?
            };
            println!("{}", serde_json::to_string_pretty(&res["data"])?);
            if res["ok"] != json!(true) {
                std::process::exit(2);
            }
            Ok(())
        }
        Command::Topics {
            pattern,
            watch,
            json,
        } => {
            let client = client::Client::discover()?;
            let q = pattern
                .as_deref()
                .map(|p| format!("?topic={}", client::encode_segment(p)))
                .unwrap_or_default();
            if watch {
                return client.get_events(&format!("/v1/topics/watch{q}"), |_, s| {
                    if json {
                        println!("{s}");
                    } else {
                        println!(
                            "{} {} #{} {}",
                            cyan(s["topic"].as_str().unwrap_or("?")),
                            dim(s["from_name"].as_str().unwrap_or("?")),
                            s["seq"],
                            ops::compact(&s["data"], 500)
                        );
                    }
                });
            }
            let list = client.get(&format!("/v1/topics{q}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                for s in list.as_array().cloned().unwrap_or_default() {
                    println!(
                        "{} {} #{} {} {}",
                        cyan(s["topic"].as_str().unwrap_or("?")),
                        dim(s["from_name"].as_str().unwrap_or("?")),
                        s["seq"],
                        dim(&format!(
                            "{}s ago",
                            s["age_ms"].as_u64().unwrap_or(0) / 1000
                        )),
                        ops::compact(&s["data"], 500)
                    );
                }
            }
            Ok(())
        }
        Command::Publish { topic, data, hops } => {
            let data: Value = serde_json::from_str(&data).unwrap_or(Value::String(data));
            let res = client::Client::discover()?.post(
                "/v1/publish",
                &json!({"topic": topic, "data": data, "hops": hops}),
            )?;
            println!("published {topic} #{}", res["seq"]);
            Ok(())
        }
        Command::Stop { robot, reason } => {
            let res = client::Client::discover()?
                .post("/v1/estop", &json!({"to": robot, "reason": reason}))?;
            println!("{}", red(res["status"].as_str().unwrap_or("stop sent")));
            Ok(())
        }
        Command::SendFile { to, path } => {
            let path = std::fs::canonicalize(&path)
                .with_context(|| format!("cannot open {}", path.display()))?;
            let res = client::Client::discover()?.post(
                "/v1/files",
                &json!({"to": to, "path": path.display().to_string()}),
            )?;
            let receipt = &res["receipt"];
            println!(
                "{} sent {} ({}) to {to} in {}ms, verified",
                green("✓"),
                receipt["name"].as_str().unwrap_or("?"),
                ops::human_size(receipt["size"].as_u64().unwrap_or(0)),
                receipt["ms"]
            );
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
        Command::Bench => bench::run().await,
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
            Some("stop") => println!(
                "{} {}: {}",
                red("[EMERGENCY STOP]"),
                bold(item["from_name"].as_str().unwrap_or("?")),
                item["reason"].as_str().unwrap_or("")
            ),
            Some("file") => println!(
                "{} {} sent {} ({}) → {}",
                green("[file]"),
                bold(item["from_name"].as_str().unwrap_or("?")),
                item["name"].as_str().unwrap_or("?"),
                ops::human_size(item["size"].as_u64().unwrap_or(0)),
                item["path"].as_str().unwrap_or("")
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

/// A node serving the HTTP API, ready to run.
pub struct Daemon {
    pub listener: tokio::net::TcpListener,
    pub node: config::RunningNode,
    pub agent: Arc<amara_agent::Agent>,
    pub token: String,
    pub url: String,
}

/// Bind the API, start the node and record it in `~/.amara/daemon.json`.
pub async fn start_daemon(args: &NodeArgs, api: Option<&str>) -> Result<Daemon> {
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
    let url = format!("http://{addr}");
    let info = DaemonInfo {
        url: url.clone(),
        name: id.display_name().to_string(),
        id: id.peer_id().to_string(),
        pid: std::process::id(),
    };
    write_private(&daemon_path(), &serde_json::to_string_pretty(&info)?)?;
    Ok(Daemon {
        listener,
        node,
        agent,
        token,
        url,
    })
}

/// Serve until `shutdown` resolves, then clean up.
pub async fn serve_daemon(
    daemon: Daemon,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let app = api::router(daemon.agent.clone(), daemon.token);
    let result = axum::serve(daemon.listener, app)
        .with_graceful_shutdown(shutdown)
        .await;
    let _ = std::fs::remove_file(daemon_path());
    daemon.agent.shutdown();
    drop(daemon.node);
    result.context("HTTP API failed")
}

async fn up(args: &NodeArgs, api: Option<&str>) -> Result<()> {
    let daemon = start_daemon(args, api).await?;
    let id = daemon.agent.identity();
    let lan = daemon
        .listener
        .local_addr()
        .is_ok_and(|a| a.ip().is_unspecified());
    eprintln!(
        "\n  {} {} {}\n  {} {}  {}\n  {} {}/ui\n  {} {}\n",
        bold("AMARA"),
        bold(id.display_name()),
        dim(&id.peer_id().to_string()),
        dim("api "),
        daemon.url,
        dim("(Authorization: Bearer <~/.amara/api.token>)"),
        dim("ui  "),
        daemon.url,
        dim("mesh"),
        dim(if lan {
            "LAN discovery on; API open to your LAN (token required). `amara pair` for phones. Ctrl-C to stop"
        } else {
            "LAN multicast discovery on; Ctrl-C to stop"
        })
    );
    serve_daemon(daemon, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// The machine's LAN address (no packets are sent).
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket
        .local_addr()
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_loopback())
}

/// Dashboard link carrying the token in the URL fragment (never sent to servers).
pub fn dashboard_link(base: &str, token: &str) -> String {
    format!("{}/ui#token={token}", base.trim_end_matches('/'))
}

fn pair() -> Result<()> {
    let info: DaemonInfo = std::fs::read_to_string(daemon_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or_else(|| anyhow::anyhow!("no running node. Start one with `amara up --lan`"))?;
    let addr: std::net::SocketAddr = info
        .url
        .trim_start_matches("http://")
        .parse()
        .context("daemon url")?;
    if !addr.ip().is_unspecified() {
        anyhow::bail!(
            "the API only listens on {} (this machine). Restart it with `amara up --lan` so phones can reach it",
            addr.ip()
        );
    }
    let ip = lan_ip().ok_or_else(|| anyhow::anyhow!("no LAN address found"))?;
    let link = dashboard_link(&format!("http://{ip}:{}", addr.port()), &api_token()?);
    let code = qrcode::QrCode::new(link.as_bytes())?;
    println!(
        "{}",
        code.render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build()
    );
    println!("\n  Scan with your phone (same Wi-Fi), or open:\n  {link}\n");
    println!(
        "  {}",
        dim("The link contains your API token: anyone with it controls this agent.")
    );
    Ok(())
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
