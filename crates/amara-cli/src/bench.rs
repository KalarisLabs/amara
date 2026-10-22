//! `amara bench`: measure the stack end to end on this machine. Everything
//! runs through the real mesh (signatures, sealing, framing, relaying) over the
//! in-process simulator with zero link latency, so the numbers are the cost of
//! AMARA itself.

use crate::simulate::agent;
use crate::style::{bold, dim, green, magenta};
use amara_agent::{Agent, ToolSpec};
use amara_transport::{LinkConfig, SimulatorHub};
use anyhow::{Result, bail};
use serde_json::json;
use std::time::{Duration, Instant};

const INSTANT: LinkConfig = LinkConfig {
    latency_min_ms: 0,
    latency_max_ms: 0,
    drop_probability: 0.0,
};

fn row(what: &str, value: String, note: &str) {
    println!("  {:<34} {:>14}  {}", what, bold(&value), dim(note));
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

async fn mesh() -> Result<(SimulatorHub, Agent, Agent, Agent)> {
    let hub = SimulatorHub::new();
    // Flood limits are off so the numbers show the stack, not the policy.
    let a = agent(&hub, "caller", |b| b.packets_per_sec(u32::MAX)).await?;
    let r = agent(&hub, "relay", |b| b.packets_per_sec(u32::MAX)).await?;
    let dir = std::env::temp_dir().join(format!("amara-bench-{}", std::process::id()));
    let s = agent(&hub, "server", move |b| {
        b.files(amara_agent::FilePolicy {
            dir: Some(dir),
            max_size: 256 * 1024 * 1024,
            ..Default::default()
        })
        .calls_per_minute(u32::MAX)
        .packets_per_sec(u32::MAX)
        .tool_fn(
            ToolSpec::new("echo", "Echo"),
            |ctx| async move { Ok(ctx.args) },
        )
        .tool_fn(
            ToolSpec::new("count", "Stream n chunks"),
            |ctx| async move {
                let n = ctx.args["n"].as_u64().unwrap_or(0);
                for i in 0..n {
                    ctx.stream.send(json!(i));
                }
                Ok(json!(n))
            },
        )
    })
    .await?;
    hub.connect(a.identity().peer_id(), r.identity().peer_id(), INSTANT)
        .await;
    hub.connect(r.identity().peer_id(), s.identity().peer_id(), INSTANT)
        .await;
    if a.wait_for_peers(2, Duration::from_secs(10)).await < 2
        || s.wait_for_peers(2, Duration::from_secs(10)).await < 2
    {
        bail!("bench mesh did not converge");
    }
    Ok((hub, a, r, s))
}

pub async fn run() -> Result<()> {
    println!(
        "\n  {}  {}\n",
        bold(&magenta("AMARA bench")),
        dim(
            "caller ⇄ relay ⇄ server · every packet signed, sealed, framed and relayed · flood limits off"
        )
    );
    let t = Instant::now();
    let (_hub, a, _r, s) = mesh().await?;
    row(
        "discovery (3 nodes)",
        format!("{} ms", t.elapsed().as_millis()),
        "cold start to full mesh",
    );
    let server = s.identity().peer_id();

    // Latency: sequential round trips over two hops.
    let mut samples = Vec::new();
    for i in 0..300 {
        let t = Instant::now();
        let reply = a.call(server, "echo", json!({"i": i}), None).await?;
        if !reply.ok {
            bail!("echo failed");
        }
        samples.push(t.elapsed());
    }
    samples.sort();
    row(
        "call round trip, 2 hops (p50)",
        format!("{:.2} ms", percentile(&samples, 0.5).as_secs_f64() * 1000.0),
        "seal + sign + relay + open, both ways",
    );
    row(
        "call round trip, 2 hops (p99)",
        format!(
            "{:.2} ms",
            percentile(&samples, 0.99).as_secs_f64() * 1000.0
        ),
        "",
    );

    // Throughput: many calls in flight.
    let n = 2000;
    let t = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..n {
        let a = a.clone();
        tasks.push(tokio::spawn(async move {
            a.call(
                server,
                "echo",
                json!({"i": i}),
                Some(Duration::from_secs(30)),
            )
            .await
        }));
    }
    let mut ok = 0;
    for task in tasks {
        if task.await.ok().and_then(|r| r.ok()).is_some_and(|r| r.ok) {
            ok += 1;
        }
    }
    let secs = t.elapsed().as_secs_f64();
    row(
        "concurrent calls",
        format!("{:.0} /s", ok as f64 / secs),
        &format!("{ok}/{n} completed"),
    );

    // Streaming.
    let chunks = 2000u64;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let t = Instant::now();
    let reply = a
        .call_streaming(server, "count", json!({"n": chunks}), None, tx)
        .await?;
    let mut got = 0u64;
    while rx.try_recv().is_ok() {
        got += 1;
    }
    row(
        "streamed chunks",
        format!("{:.0} /s", got as f64 / t.elapsed().as_secs_f64()),
        &format!("{got}/{} in order", reply.chunks),
    );

    // File transfer (paced to protect relays, so this is the protocol's ceiling).
    let size = 4 * 1024 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let t = Instant::now();
    let receipt = a.send_file_bytes(server, "bench.bin", data).await?;
    row(
        "file transfer, 2 hops",
        format!(
            "{:.2} MB/s",
            receipt.size as f64 / 1_048_576.0 / t.elapsed().as_secs_f64()
        ),
        "4 MiB, BLAKE3-verified, paced at 50 chunks/s",
    );

    // Topic fan-out.
    let mut feed = a.subscribe_topics();
    let t = Instant::now();
    let samples = 200;
    for i in 0..samples {
        s.publish("bench/tick", json!(i), None).await?;
    }
    let mut seen = 0;
    while seen < samples
        && let Ok(Ok(sample)) = tokio::time::timeout(Duration::from_secs(2), feed.recv()).await
    {
        if sample.topic == "bench/tick" {
            seen += 1;
        }
    }
    row(
        "topic samples delivered",
        format!("{:.0} /s", seen as f64 / t.elapsed().as_secs_f64()),
        &format!("{seen}/{samples} across 2 hops"),
    );

    println!(
        "\n  {} {}\n",
        green("●"),
        dim("Real links add their own latency: LAN ≈ 1 ms per hop, BLE ≈ 15-50 ms per hop.")
    );
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("amara-bench-{}", std::process::id())),
    );
    Ok(())
}
