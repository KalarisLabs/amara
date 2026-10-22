//! `amara simulate`: a three-agent mesh in one process, no network needed.

use crate::ops;
use crate::style::{bold, cyan, dim, green, magenta};
use amara_agent::{Agent, ToolSpec};
use amara_transport::{LinkConfig, SimulatedTransport, SimulatorHub};
use anyhow::{Result, bail};
use serde_json::json;
use std::time::{Duration, Instant};

async fn agent(
    hub: &SimulatorHub,
    name: &str,
    build: impl FnOnce(amara_agent::AgentBuilder) -> amara_agent::AgentBuilder,
) -> Result<Agent> {
    let identity = amara_crypto::identity::NodeIdentity::generate(name);
    let transport = SimulatedTransport::new(identity.peer_id(), hub.clone()).await;
    Ok(build(
        Agent::builder(name)
            .identity(identity)
            .transport(Box::new(transport)),
    )
    .start()
    .await?)
}

pub async fn run() -> Result<()> {
    println!(
        "\n  {}  {}\n",
        bold(&magenta("AMARA simulate")),
        dim("alice ⇄ bob ⇄ carol — alice and carol are out of range")
    );
    let hub = SimulatorHub::new();
    let link = LinkConfig {
        latency_min_ms: 5,
        latency_max_ms: 20,
        drop_probability: 0.0,
    };

    let alice = agent(&hub, "alice", |b| b).await?;
    let bob = agent(&hub, "bob", |b| b.tag("relay")).await?;
    let carol =
        agent(&hub, "carol", |b| {
            b.tag("reasoning").tool_fn(
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
        })
        .await?;

    hub.connect(alice.identity().peer_id(), bob.identity().peer_id(), link)
        .await;
    hub.connect(bob.identity().peer_id(), carol.identity().peer_id(), link)
        .await;

    let t = Instant::now();
    let converged = alice.wait_for_peers(2, Duration::from_secs(10)).await >= 2
        && bob.wait_for_peers(2, Duration::from_secs(10)).await >= 2
        && carol.wait_for_peers(2, Duration::from_secs(10)).await >= 2;
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

    println!(
        "\n{} alice → call summarize (no address: routed by capability)",
        cyan("›")
    );
    let t = Instant::now();
    let out = alice
        .call_tool(None, "summarize", json!({"text": "Agents should be able to meet physically. Connectivity should emerge from proximity rather than infrastructure."}))
        .await?;
    println!(
        "  {} {} {}",
        green("✓"),
        dim(&format!(
            "{}ms via bob (2 hops, sealed end-to-end to carol)",
            t.elapsed().as_millis()
        )),
        out
    );

    println!("\n{} alice → broadcast on #lab", cyan("›"));
    alice
        .broadcast_message("hello mesh 👋", Some("#lab".into()))
        .await?;
    let got = carol.inbox().take(5, Duration::from_secs(5)).await;
    println!(
        "  {} carol received: {}",
        green("✓"),
        ops::render_inbox(&got)
    );

    println!("\n{} carol → encrypted DM to alice", cyan("›"));
    carol
        .send_message(alice.identity().peer_id(), "your summary is ready")
        .await?;
    let got = alice.inbox().take(5, Duration::from_secs(5)).await;
    println!(
        "  {} alice received: {}",
        green("✓"),
        ops::render_inbox(&got)
    );
    let _ = bob.inbox().take(10, Duration::from_millis(200)).await;
    println!(
        "\n{}\n",
        dim(
            "bob relayed every packet but holds no key for the call or the DM. That's the whole point."
        )
    );
    Ok(())
}
