//! `amara simulate`: a mesh of agents and a robot in one process, no network
//! needed. Shows multi-hop sealed calls, streaming, live topics, store and
//! forward, file transfer and the emergency stop.

use crate::ops;
use crate::style::{bold, cyan, dim, green, magenta, red};
use amara_agent::{Agent, AgentBuilder, InboxItem, ToolSpec};
use amara_transport::{LinkConfig, SimulatedTransport, SimulatorHub};
use anyhow::{Result, bail};
use serde_json::json;
use std::time::{Duration, Instant};

pub async fn agent(
    hub: &SimulatorHub,
    name: &str,
    build: impl FnOnce(AgentBuilder) -> AgentBuilder,
) -> Result<Agent> {
    let identity = amara_crypto::identity::NodeIdentity::generate(name);
    let transport = SimulatedTransport::new(identity.peer_id(), hub.clone()).await;
    Ok(build(
        Agent::builder(name)
            .identity(identity)
            .ephemeral()
            .transport(Box::new(transport)),
    )
    .start()
    .await?)
}

fn step(text: &str) {
    println!("\n{} {text}", cyan("›"));
}

pub async fn run() -> Result<()> {
    println!(
        "\n  {}  {}\n",
        bold(&magenta("AMARA simulate")),
        dim("alice ⇄ bob ⇄ rover — alice and the rover are out of each other's range")
    );
    let hub = SimulatorHub::new();
    let link = LinkConfig {
        latency_min_ms: 5,
        latency_max_ms: 20,
        drop_probability: 0.0,
    };
    let files = std::env::temp_dir().join(format!("amara-simulate-{}", std::process::id()));

    let alice = agent(&hub, "alice", |b| {
        b.files(amara_agent::FilePolicy {
            dir: Some(files.clone()),
            ..Default::default()
        })
    })
    .await?;
    let bob = agent(&hub, "bob", |b| b.tag("relay")).await?;
    let rover = agent(&hub, "rover", |b| {
        b.tag("robot")
            .tool_fn(
                ToolSpec::new("summarize", "Summarize text in one line").with_schema(json!({
                    "type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}
                })),
                |ctx| async move {
                    let text = ctx.args["text"].as_str().ok_or("text is required")?;
                    let words = text.split_whitespace().count();
                    let first: String = text.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
                    Ok(json!({"summary": format!("{first}… ({words} words)"), "for": ctx.caller_name}))
                },
            )
            .tool_fn(
                ToolSpec::new("drive_to", "Drive to a waypoint, streaming progress"),
                |ctx| async move {
                    let target = ctx.args["waypoint"].as_str().unwrap_or("dock").to_string();
                    for pct in [25, 50, 75, 100] {
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        ctx.stream.send(json!({"waypoint": target, "progress": pct}));
                    }
                    Ok(json!({"arrived": target}))
                },
            )
    })
    .await?;

    let (ia, ib, ir) = (
        alice.identity().peer_id(),
        bob.identity().peer_id(),
        rover.identity().peer_id(),
    );
    hub.connect(ia, ib, link).await;
    hub.connect(ib, ir, link).await;

    let t = Instant::now();
    let converged = alice.wait_for_peers(2, Duration::from_secs(10)).await >= 2
        && bob.wait_for_peers(2, Duration::from_secs(10)).await >= 2
        && rover.wait_for_peers(2, Duration::from_secs(10)).await >= 2;
    if !converged {
        bail!("discovery did not converge");
    }
    println!(
        "{} mesh converged in {}ms — alice's view:\n",
        green("●"),
        t.elapsed().as_millis()
    );
    for line in ops::render_peers(&alice, &alice.peers()).lines() {
        println!("    {line}");
    }

    step("alice → call summarize (no address: routed by capability)");
    let t = Instant::now();
    let out = alice
        .call_tool(None, "summarize", json!({"text": "Agents should be able to meet physically. Connectivity should emerge from proximity rather than infrastructure."}))
        .await?;
    println!(
        "  {} {} {}",
        green("✓"),
        dim(&format!(
            "{}ms via bob (2 hops, sealed end-to-end to the rover)",
            t.elapsed().as_millis()
        )),
        out
    );

    step("alice → drive_to, streaming progress like a robot action");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            println!("  {} {}", dim("…"), chunk);
        }
    });
    let reply = alice
        .call_streaming(ir, "drive_to", json!({"waypoint": "bay-3"}), None, tx)
        .await?;
    let _ = printer.await;
    println!("  {} {}", green("✓"), reply.data);

    step("rover → publishes live telemetry; alice reads the latest value");
    for pct in [87, 86] {
        rover
            .publish("rover/battery", json!({"pct": pct}), None)
            .await?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while alice.topics(Some("rover/*")).first().map(|s| s.seq) != Some(2)
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "  {} {}",
        green("✓"),
        ops::render_topics(&alice.topics(Some("rover/*")))
    );

    step("alice → encrypted DM to the rover while it is out of range (store and forward)");
    hub.disconnect(ib, ir).await;
    alice.mesh().registry().remove_peer(&ir);
    bob.mesh().registry().remove_peer(&ir);
    println!(
        "  {} {}",
        dim("·"),
        ops::send(&alice, Some("rover"), "charge before the next run", None).await?
    );
    hub.disconnect(ia, ib).await;
    println!(
        "  {} {}",
        dim("·"),
        dim("alice leaves; bob holds the sealed message")
    );
    hub.connect(ib, ir, link).await;
    let got = rover.inbox().take(1, Duration::from_secs(5)).await;
    println!(
        "  {} rover received: {}",
        green("✓"),
        ops::render_inbox(&got)
    );
    hub.connect(ia, ib, link).await;
    alice.wait_for_peers(2, Duration::from_secs(5)).await;

    step("rover → sends alice its map as a file (verified by hash)");
    let map: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let receipt = rover.send_file_bytes(ia, "map.bin", map).await?;
    println!(
        "  {} {}",
        green("✓"),
        ops::render_receipt("alice", &receipt)
    );

    step("alice → EMERGENCY STOP to every robot in range");
    alice.estop(None, "person in the aisle").await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let items = rover.inbox().take(10, Duration::from_millis(200)).await;
        if let Some(InboxItem::Stop {
            reason, from_name, ..
        }) = items.iter().find(|i| matches!(i, InboxItem::Stop { .. }))
        {
            println!("  {} rover halted: {} (from {from_name})", red("■"), reason);
            break;
        }
        if Instant::now() > deadline {
            bail!("the stop did not arrive");
        }
    }
    let _ = std::fs::remove_dir_all(&files);
    println!(
        "\n{}\n",
        dim(
            "bob relayed and held packets but could read none of them: calls, DMs and files are sealed end to end."
        )
    );
    Ok(())
}
