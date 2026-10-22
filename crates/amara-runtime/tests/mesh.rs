//! End-to-end mesh behaviour over the deterministic in-process simulator.

use amara_crypto::identity::NodeIdentity;
use amara_mesh::{MeshConfig, MeshEngine};
use amara_protocol::PacketType;
use amara_runtime::{AgentRuntime, FnTool, InboxItem, RuntimeConfig, ToolSpec};
use amara_transport::{LinkConfig, SimulatedTransport, SimulatorHub};
use serde_json::json;
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
    (a, b, c)
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
