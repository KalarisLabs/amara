# AMARA Architecture

## Overview

AMARA is a five-layer decentralized proximity network for AI agents. Each layer has strict boundaries — no layer may depend on the internals of a non-adjacent layer.

## Layer Diagram

```text
┌──────────────────────────────────────────────────────────────┐
│  Layer 5: UX                                                  │
│  CLI · TUI · Desktop GUI · Local HTTP API                     │
│  Presents: peers, capabilities, tasks, results                │
│  Depends on: Agent Runtime (Layer 4)                          │
├──────────────────────────────────────────────────────────────┤
│  Layer 4: Agent Runtime                                       │
│  Task routing · delegation · tool execution · model backends  │
│  Manages: agent lifecycle, capability registration, scoring   │
│  Depends on: Protocol (Layer 3)                               │
├──────────────────────────────────────────────────────────────┤
│  Layer 3: Protocol                                            │
│  Packet types · encoding · framing · versioning               │
│  Defines: ANNOUNCE, TASK, RESULT, CAPABILITY, etc.            │
│  Depends on: Mesh Engine (Layer 2)                            │
├──────────────────────────────────────────────────────────────┤
│  Layer 2: Mesh Engine                                         │
│  Discovery · routing · relay · dedup · TTL · fragmentation    │
│  Manages: peer registry, route table, message lifecycle       │
│  Depends on: Transport (Layer 1)                              │
├──────────────────────────────────────────────────────────────┤
│  Layer 1: Link Transport                                      │
│  BLE · Simulated · Loopback · (future: Wi-Fi, LAN, USB)      │
│  Provides: send/receive bytes, scan, advertise, connect       │
│  Depends on: platform hardware                                │
└──────────────────────────────────────────────────────────────┘
```

## Crate Structure

```text
amara/
├── crates/
│   ├── amara-core/          # Shared types, errors, constants, PeerId
│   ├── amara-crypto/        # Ed25519, X25519, Noise, encryption
│   ├── amara-protocol/      # Packet encoding/decoding, framing
│   ├── amara-mesh/          # Mesh engine, routing, relay, dedup, fragmentation
│   ├── amara-transport/     # Transport trait + SimulatedTransport + LoopbackTransport
│   ├── amara-runtime/       # Agent runtime, task routing, capability registry
│   ├── amara-agent/         # Agent SDK, model backends, tool system
│   └── amara-cli/           # CLI binary, TUI, interactive mode
├── apps/
│   └── desktop/             # Tauri desktop application
├── sdk/
│   └── python/              # Python SDK
├── docs/
├── tests/                   # Integration tests
├── fuzz/                    # Fuzz targets
├── examples/
├── scripts/
└── .github/
```

## Dependency Graph

```text
amara-cli
  └── amara-runtime
        ├── amara-agent
        │     └── amara-core
        ├── amara-protocol
        │     ├── amara-crypto
        │     │     └── amara-core
        │     └── amara-core
        └── amara-mesh
              ├── amara-transport
              │     └── amara-core
              ├── amara-protocol
              └── amara-core
```

## Identity Model

Every AMARA node has a cryptographic identity generated at first run:

- **Ed25519 signing key pair** — signs all outgoing packets
- **X25519 key pair** — derived for Diffie-Hellman key agreement
- **PeerId** — BLAKE3 hash of the public key, truncated to 8 bytes (displayed as hex)
- **Display name** — human-friendly, non-authoritative label

Identity is stored locally in `~/.amara/identity.key` (encrypted at rest where platform supports).

## Peer Registry

Each node maintains an ephemeral local registry:

| Field | Description |
|-------|-------------|
| peer_id | Cryptographic identifier |
| display_name | Human-readable name |
| public_key | Ed25519 public key |
| capabilities | Advertised capability list |
| hop_count | Estimated distance |
| last_seen | Timestamp of last activity |
| trust_level | UNKNOWN / SEEN / VERIFIED / TRUSTED / BLOCKED |
| transport | Which transport discovered this peer |
| load_class | IDLE / LOW / MEDIUM / HIGH / OVERLOADED |
| version | Protocol version |
| route | Next-hop peer for reaching this peer |

Entries expire after configurable timeout (default: 5 minutes without heartbeat).

## Transport Abstraction

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
    async fn send(&self, destination: &TransportAddr, data: &[u8]) -> Result<()>;
    async fn broadcast(&self, data: &[u8]) -> Result<()>;
    async fn events(&self) -> broadcast::Receiver<TransportEvent>;
    fn name(&self) -> &str;
}
```

Three transports are implemented in Phase 1:

1. **SimulatedTransport** — deterministic mesh simulator for testing
2. **LoopbackTransport** — single-machine localhost communication
3. **BleTransport** — real Bluetooth Low Energy (Phase 2)

## Mesh Engine

The mesh engine handles:

- **Deduplication**: Message IDs tracked in a bounded LRU set
- **TTL**: Each hop decrements; drop at 0
- **Relay**: Forward eligible messages to other peers
- **Relay jitter**: Random delay before relay to prevent storms
- **Split horizon**: Don't relay back to the transport that delivered the message
- **Fragmentation**: Large messages split into 500-byte chunks, reassembled with timeout
- **Topology**: Peer announcements build a local view of the network

## Security Model

See [SECURITY.md](SECURITY.md) for full threat model.

Key principles:
- **Proximity ≠ trust**: Physical nearness grants discovery, not authority
- **Cryptographic authentication**: All packets signed by sender
- **End-to-end encryption**: Task payloads encrypted to destination; relays see only routing metadata
- **Rate limiting**: Per-peer and per-capability quotas
- **Sandboxing**: Remote tasks execute in restricted capability scope

## Task Lifecycle

```text
1. Issuer creates TASK with capability requirement
2. Mesh resolves target (direct addressing or capability-based routing)
3. TASK sent (encrypted if private)
4. Target agent receives, validates permissions
5. TASK_ACCEPT or TASK_REJECT
6. Agent executes task in sandboxed runtime
7. RESULT (possibly chunked via RESULT_CHUNK / RESULT_COMPLETE)
8. Issuer receives and ACKs
```

## Configuration

All configuration is optional. Defaults work out of the box.

```toml
# ~/.amara/config.toml (generated on first run if needed)
name = "nova"

[network]
max_ttl = 7
relay_enabled = true
store_forward = true
store_forward_max_age_secs = 43200

[security]
require_trust_for_tasks = false
auto_trust_direct_peers = false

[agent]
capabilities = ["reasoning"]

[api]
enabled = true
bind = "127.0.0.1:7432"
```
