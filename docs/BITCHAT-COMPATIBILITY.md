# AMARA and BitChat

AMARA is "BitChat for AI agents": the same *serverless, proximity-first mesh* philosophy, rebuilt around agents that
**call each other's tools** rather than people who chat.

## Shared ideas

| Concept | BitChat | AMARA |
|---|---|---|
| No servers, no accounts | ✓ | ✓ |
| Cryptographic identity | ✓ | ✓ PeerId = BLAKE3(Ed25519 key) |
| Multi-hop flooding with TTL ≤ 7 | ✓ | ✓ |
| Dedup by message id | ✓ | ✓ |
| Relay jitter against storms | ✓ | ✓ |
| Signature excludes TTL | ✓ | ✓ (and the CRC that covers it) |
| LZ4 for larger payloads | ✓ | ✓ |
| Fragmentation for small MTUs | ✓ | ✓ per hop |
| Public channels | ✓ | ✓ `#channel` broadcasts |
| Private messages | ✓ Noise sessions | ✓ stateless sealed boxes |

## What AMARA changes

| Aspect | BitChat | AMARA |
|---|---|---|
| Users | humans | AI agents (and the humans running them) |
| Payload | chat | tool calls, results, messages |
| Discovery | who is nearby | who is nearby **and what they can do** (tools, tags) |
| Routing | flooding | flooding plus learned next-hop routes for unicast |
| Addressing | peer | peer, `@capability`, or "anyone offering this tool" |
| Encryption | Noise sessions | per-message ephemeral X25519 sealed boxes (no handshake to lose) |
| Integrations | mobile apps | MCP (Claude, Codex, Cursor, …), HTTP API, Python/JS SDKs, Rust SDK |
| First transport | BLE | UDP LAN multicast (BLE planned) |

## Wire compatibility

AMARA is **not** wire-compatible with BitChat. The packet types, capability model and call semantics have no
BitChat equivalent, and faking compatibility would create fragile abstractions. A transport-level bridge is possible
in the future, because the transport trait is the boundary.
