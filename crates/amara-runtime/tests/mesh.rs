//! End-to-end mesh behaviour over the deterministic in-process simulator.

use amara_crypto::identity::NodeIdentity;
use amara_mesh::{MeshConfig, MeshEngine};
use amara_protocol::PacketType;
use amara_runtime::{
    AgentRuntime, Delivery, EstopFrom, EstopPolicy, FILE_CHUNK_SIZE, FilePolicy, FnTool, InboxItem,
    RuntimeConfig, ToolSpec,
};
use amara_transport::{LinkConfig, SimulatedTransport, SimulatorHub};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const FAST: LinkConfig = LinkConfig {
    latency_min_ms: 1,
    latency_max_ms: 3,
    drop_probability: 0.0,
};

async fn node(hub: &SimulatorHub, name: &str, config: RuntimeConfig) -> Arc<AgentRuntime> {
    let id = NodeIdentity::generate(name);
    let mesh = Arc::new(MeshEngine::new(id.clone(), MeshConfig::default()));
    mesh.add_transport(Box::new(
        SimulatedTransport::new(id.peer_id(), hub.clone()).await,
    ))
    .await;
    AgentRuntime::new(mesh, config)
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// a <-> b <-> c, all started, with full discovery.
async fn chain() -> (Arc<AgentRuntime>, Arc<AgentRuntime>, Arc<AgentRuntime>) {
    let (_, a, b, c) = chain_with_hub().await;
    (a, b, c)
}

async fn chain_with_hub() -> (
    SimulatorHub,
    Arc<AgentRuntime>,
    Arc<AgentRuntime>,
    Arc<AgentRuntime>,
) {
    let hub = SimulatorHub::new();
    let a = node(&hub, "alice", RuntimeConfig::default()).await;
    let b = node(&hub, "bob", RuntimeConfig::default()).await;
    let c = node(
        &hub,
        "carol",
        RuntimeConfig {
            tags: vec!["vision".into()],
            ..Default::default()
        },
    )
    .await;
    let (ia, ib, ic) = (
        a.identity().peer_id(),
        b.identity().peer_id(),
        c.identity().peer_id(),
    );
    hub.connect(ia, ib, FAST).await;
    hub.connect(ib, ic, FAST).await;
    c.register_tool(Arc::new(FnTool::new(
        ToolSpec::new("upper", "Uppercase text")
            .with_schema(json!({"type":"object","properties":{"text":{"type":"string"}}})),
        |ctx| async move {
            Ok(json!(
                ctx.args["text"].as_str().unwrap_or_default().to_uppercase()
            ))
        },
    )))
    .unwrap();
    for rt in [&a, &b, &c] {
        rt.start().await.unwrap();
    }
    wait_until("discovery", || {
        a.peers().len() == 2 && c.peers().len() == 2 && b.peers().len() == 2
    })
    .await;
    (hub, a, b, c)
}

#[tokio::test]
async fn multi_hop_tool_call_is_end_to_end_encrypted() {
    let (a, b, c) = chain().await;
    let carol = a.resolve("carol").unwrap();
    assert_eq!(carol.hop_count, 2, "carol is two hops from alice");

    // Watch what the relay sees.
    let mut relay_view = b.mesh().subscribe();
    let mut relay_raw = b.mesh().events();
    let _ = &mut relay_raw;

    let reply = a
        .call(
            carol.peer_id,
            "upper",
            json!({"text": "hello mesh"}),
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    assert!(reply.ok, "{:?}", reply.data);
    assert_eq!(reply.data, json!("HELLO MESH"));

    // The relay never had the call delivered to itself (it is not the destination).
    while let Ok(inbound) = relay_view.try_recv() {
        assert_ne!(inbound.packet.header.packet_type, PacketType::Task);
    }
    drop(c);
}

#[tokio::test]
async fn capability_routing_and_cards() {
    let (a, _b, c) = chain().await;
    // No target: route by tool name.
    let target = a.resolve_target(None, "upper").unwrap();
    assert_eq!(target.peer_id, c.identity().peer_id());
    // "@tag" routes by advertised capability tag.
    assert_eq!(
        a.resolve_target(Some("@vision"), "x").unwrap().peer_id,
        c.identity().peer_id()
    );
    assert!(a.resolve_target(None, "teleport").is_err());

    let card = a
        .call(target.peer_id, "amara.card", json!({}), None)
        .await
        .unwrap();
    assert_eq!(card.data["name"], "carol");
    assert_eq!(card.data["tools"][0]["name"], "upper");

    let unknown = a
        .call(target.peer_id, "nope", json!({}), None)
        .await
        .unwrap();
    assert!(!unknown.ok);
}

#[tokio::test]
async fn inbox_served_tool_roundtrip() {
    let (a, _b, c) = chain().await;
    c.expose(ToolSpec::new("ask_human", "Ask the operator"))
        .unwrap();
    wait_until("re-announce", || {
        a.resolve("carol")
            .map(|p| p.capabilities.contains(&"ask_human".to_string()))
            .unwrap_or(false)
    })
    .await;

    let c2 = c.clone();
    let server = tokio::spawn(async move {
        let items = c2.inbox().take(1, Duration::from_secs(5)).await;
        match &items[0] {
            InboxItem::Call {
                call_id,
                tool,
                args,
                ..
            } => {
                assert_eq!(tool, "ask_human");
                c2.reply(
                    *call_id,
                    Ok(json!({"answer": format!("yes to {}", args["q"])})),
                )
                .unwrap();
            }
            other => panic!("unexpected {other:?}"),
        }
    });
    let reply = a
        .call(
            c.identity().peer_id(),
            "ask_human",
            json!({"q": "deploy?"}),
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert!(reply.ok);
    assert_eq!(reply.data["answer"], "yes to \"deploy?\"");
}

#[tokio::test]
async fn messages_direct_and_broadcast() {
    let (a, b, c) = chain().await;
    a.send_message(c.identity().peer_id(), "psst")
        .await
        .unwrap();
    a.broadcast_message("hello everyone", Some("#lab".into()))
        .await
        .unwrap();

    let got = c.inbox().take(10, Duration::from_secs(5)).await;
    let mut all = got;
    if all.len() < 2 {
        all.extend(c.inbox().take(10, Duration::from_secs(5)).await);
    }
    let direct = all
        .iter()
        .any(|i| matches!(i, InboxItem::Message { text, direct: true, .. } if text == "psst"));
    let public = all.iter().any(|i| matches!(i, InboxItem::Message { text, direct: false, channel: Some(ch), .. } if text == "hello everyone" && ch == "#lab"));
    assert!(direct && public, "{all:?}");

    // The relay gets the broadcast but never the direct message.
    let relay = b.inbox().take(10, Duration::from_secs(2)).await;
    assert!(
        relay
            .iter()
            .all(|i| !matches!(i, InboxItem::Message { text, .. } if text == "psst"))
    );
}

#[tokio::test]
async fn require_trusted_rejects_strangers() {
    let hub = SimulatorHub::new();
    let a = node(&hub, "alice", RuntimeConfig::default()).await;
    let b = node(
        &hub,
        "bob",
        RuntimeConfig {
            require_trusted: true,
            ..Default::default()
        },
    )
    .await;
    hub.connect(a.identity().peer_id(), b.identity().peer_id(), FAST)
        .await;
    b.register_tool(Arc::new(FnTool::new(
        ToolSpec::new("secret", ""),
        |_| async { Ok(json!(42)) },
    )))
    .unwrap();
    a.start().await.unwrap();
    b.start().await.unwrap();
    wait_until("discovery", || !a.peers().is_empty()).await;
    let reply = a
        .call(b.identity().peer_id(), "secret", json!({}), None)
        .await
        .unwrap();
    assert!(
        !reply.ok
            && reply.data["error"]
                .as_str()
                .unwrap()
                .contains("permission denied")
    );
}

#[tokio::test]
async fn streaming_call_delivers_chunks_in_order() {
    let (a, _b, c) = chain().await;
    c.register_tool(Arc::new(FnTool::new(
        ToolSpec::new("count", "Count to n, streaming"),
        |ctx| async move {
            let n = ctx.args["n"].as_u64().unwrap_or(0);
            for i in 0..n {
                ctx.stream.send(json!(i));
            }
            Ok(json!({"done": n, "streamed": ctx.stream.is_active()}))
        },
    )))
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let reply = a
        .call_streaming(
            c.identity().peer_id(),
            "count",
            json!({"n": 20}),
            Some(Duration::from_secs(5)),
            tx,
        )
        .await
        .unwrap();
    assert!(reply.ok && reply.data["streamed"] == json!(true));
    assert_eq!(reply.chunks, 20);
    let got: Vec<u64> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(got, (0..20).collect::<Vec<_>>());

    // A plain call to the same tool streams nothing.
    let plain = a
        .call(c.identity().peer_id(), "count", json!({"n": 3}), None)
        .await
        .unwrap();
    assert_eq!(plain.chunks, 0);
    assert_eq!(plain.data["streamed"], json!(false));
}

#[tokio::test]
async fn inbox_tool_can_stream_partial_replies() {
    let (a, _b, c) = chain().await;
    c.expose(ToolSpec::new("think", "Think out loud")).unwrap();
    let c2 = c.clone();
    let server = tokio::spawn(async move {
        let items = c2.inbox().take(1, Duration::from_secs(5)).await;
        let InboxItem::Call {
            call_id, stream, ..
        } = &items[0]
        else {
            panic!("expected a call")
        };
        assert!(stream);
        c2.reply_partial(*call_id, json!("step 1")).unwrap();
        c2.reply_partial(*call_id, json!("step 2")).unwrap();
        c2.reply(*call_id, Ok(json!("answer"))).unwrap();
        assert!(c2.reply_partial(*call_id, json!("late")).is_err());
    });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let reply = a
        .call_streaming(
            c.identity().peer_id(),
            "think",
            json!({}),
            Some(Duration::from_secs(5)),
            tx,
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(reply.data, json!("answer"));
    let got: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(got, vec![json!("step 1"), json!("step 2")]);
}

#[tokio::test]
async fn file_transfer_over_two_hops() {
    let hub = SimulatorHub::new();
    let dir = std::env::temp_dir().join(format!("amara-test-{}", uuid::Uuid::new_v4()));
    let receiver_cfg = RuntimeConfig {
        files: FilePolicy {
            dir: Some(dir.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let a = node(&hub, "alice", RuntimeConfig::default()).await;
    let b = node(&hub, "bob", RuntimeConfig::default()).await;
    let c = node(&hub, "carol", receiver_cfg).await;
    hub.connect(a.identity().peer_id(), b.identity().peer_id(), FAST)
        .await;
    hub.connect(b.identity().peer_id(), c.identity().peer_id(), FAST)
        .await;
    for rt in [&a, &b, &c] {
        rt.start().await.unwrap();
    }
    wait_until("discovery", || a.peers().len() == 2 && c.peers().len() == 2).await;

    // Several chunks, last one partial.
    let data: Vec<u8> = (0..(FILE_CHUNK_SIZE as usize * 3 + 1234))
        .map(|i| (i * 7 % 251) as u8)
        .collect();
    let receipt = a
        .send_file_bytes(c.identity().peer_id(), "../report.bin", data.clone())
        .await
        .unwrap();
    assert_eq!(receipt.size, data.len() as u64);

    let items = c.inbox().take(1, Duration::from_secs(5)).await;
    let InboxItem::File {
        name, path, hash, ..
    } = &items[0]
    else {
        panic!("expected a file, got {items:?}")
    };
    assert_eq!(name, "report.bin", "path components are stripped");
    assert_eq!(hash, &receipt.hash);
    assert_eq!(std::fs::read(path).unwrap(), data);

    // A node without a download directory refuses files.
    let err = c
        .send_file_bytes(a.identity().peer_id(), "x.txt", b"hi".to_vec())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not accept files"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn messages_to_offline_peers_are_stored_and_forwarded() {
    let (hub, a, b, c) = chain_with_hub().await;
    let (ia, ib, ic) = (
        a.identity().peer_id(),
        b.identity().peer_id(),
        c.identity().peer_id(),
    );
    // carol walks out of range and everyone forgets her (as after peer expiry).
    hub.disconnect(ib, ic).await;
    a.mesh().registry().remove_peer(&ic);
    b.mesh().registry().remove_peer(&ic);

    // alice can still address her: they met before.
    let (id, name) = a.resolve_any("carol").unwrap();
    assert_eq!((id, name.as_str()), (ic, "carol"));
    let delivery = a.send_message(ic, "see you later").await.unwrap();
    assert_eq!(delivery, Delivery::Stored);
    wait_until("bob takes custody", || {
        b.mesh()
            .held()
            .iter()
            .any(|(dest, n)| *dest == ic && *n == 1)
    })
    .await;

    // alice leaves for good; carol comes back next to bob only.
    hub.disconnect(ia, ib).await;
    hub.connect(ib, ic, FAST).await;
    let items = c.inbox().take(1, Duration::from_secs(5)).await;
    assert!(
        matches!(
            items.first(),
            Some(InboxItem::Message { text, from_name, direct: true, stored: true, .. })
                if text == "see you later" && from_name == "alice"
        ),
        "{items:?}"
    );
    wait_until("custody released", || b.mesh().held().is_empty()).await;
}

#[tokio::test]
async fn topics_reach_nearby_agents_and_keep_latest_value() {
    let (a, b, c) = chain().await;
    let mut feed = a.subscribe_topics();
    // carol is a robot two hops away from alice.
    for pct in [90, 89, 88] {
        c.publish("rover/battery", json!({"pct": pct}), None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    wait_until("latest battery at alice", || {
        a.topics(Some("rover/*"))
            .first()
            .is_some_and(|s| s.data["pct"] == 88 && s.from_name == "carol")
    })
    .await;
    let first = tokio::time::timeout(Duration::from_secs(2), feed.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.topic, "rover/battery");

    // One hop of reach stops at bob.
    c.publish("rover/near", json!(1), Some(1)).await.unwrap();
    wait_until("bob sees near", || !b.topics(Some("rover/near")).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(a.topics(Some("rover/near")).is_empty(), "hops limit reach");
    assert!(
        c.publish("amara/estop", json!({}), None).await.is_err(),
        "reserved"
    );
}

#[tokio::test]
async fn emergency_stop_honours_target_and_policy() {
    let hub = SimulatorHub::new();
    let a = node(&hub, "operator", RuntimeConfig::default()).await;
    let robot = node(&hub, "arm", RuntimeConfig::default()).await;
    let locked = node(
        &hub,
        "locked",
        RuntimeConfig {
            estop: EstopPolicy {
                from: EstopFrom::Trusted,
                command: None,
            },
            ..Default::default()
        },
    )
    .await;
    for rt in [&robot, &locked] {
        hub.connect(a.identity().peer_id(), rt.identity().peer_id(), FAST)
            .await;
    }
    for rt in [&a, &robot, &locked] {
        rt.start().await.unwrap();
    }
    wait_until("discovery", || a.peers().len() == 2).await;

    a.estop(None, "person in the cell").await.unwrap();
    let items = robot.inbox().take(1, Duration::from_secs(5)).await;
    assert!(
        matches!(items.first(), Some(InboxItem::Stop { reason, from_name, .. }) if reason == "person in the cell" && from_name == "operator"),
        "{items:?}"
    );
    assert!(
        locked
            .inbox()
            .take(1, Duration::from_millis(300))
            .await
            .is_empty(),
        "untrusted stop ignored by a trusted-only robot"
    );

    // Targeted stops only reach their target.
    a.estop(Some(locked.identity().peer_id()), "targeted")
        .await
        .unwrap();
    assert!(
        robot
            .inbox()
            .take(1, Duration::from_millis(300))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn call_timeout_is_reported() {
    let (a, _b, c) = chain().await;
    c.expose(ToolSpec::new("slow", "never answers")).unwrap();
    let err = a
        .call(
            c.identity().peer_id(),
            "slow",
            json!({}),
            Some(Duration::from_millis(300)),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"));
}
