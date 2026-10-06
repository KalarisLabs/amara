# AMARA / BitChat Compatibility Analysis

## Overview

AMARA draws conceptual inspiration from the BitChat protocol while building an entirely separate system purpose-built for AI agent networking.

## What AMARA Reuses Conceptually

| Concept | BitChat | AMARA |
|---------|---------|-------|
| BLE proximity discovery | ✓ | ✓ |
| Multi-hop mesh relay | ✓ (up to 7 hops) | ✓ (configurable, default 7) |
| TTL-based hop limiting | ✓ | ✓ |
| Deduplication via message IDs | ✓ | ✓ |
| Relay jitter (storm prevention) | ✓ | ✓ |
| Cryptographic identity (no accounts) | ✓ | ✓ |
| Ed25519 signatures | ✓ | ✓ |
| Store-and-forward for offline peers | ✓ (12h) | ✓ (configurable) |
| Binary protocol over BLE | ✓ | ✓ |
| Transport abstraction | Partial | Full trait-based |
| Signature excludes TTL | ✓ | ✓ |
| Compact binary header | ✓ | ✓ (different layout) |
| Fragmentation/reassembly | ✓ (~500 byte chunks) | ✓ (488 byte payload) |
| LZ4 compression | ✓ (>100 bytes) | ✓ (flag-based) |
| Split horizon relay | ✓ | ✓ |
| No central server required | ✓ | ✓ |

## What AMARA Intentionally Changes

| Aspect | BitChat | AMARA |
|--------|---------|-------|
| Primary use | Human messaging | AI agent task delegation |
| Nostr integration | Core dual-transport | Not present (agent-focused) |
| Payload model | Chat messages | Structured tasks/results |
| Capability system | None | First-class capability ads |
| Task routing | N/A | Capability-based scoring |
| Agent runtime | N/A | Sandboxed execution engine |
| Multi-agent per node | N/A | Supported |
| Encryption | Noise XX | Noise NK (responder key known from ANNOUNCE) |
| Packet types | Messaging-focused | Agent protocol (TASK, RESULT, CAPABILITY) |
| Header size | Varies | Fixed 42 bytes |
| Protocol versioning | Version field | Version + feature flags |
| Trust model | Implicit | Explicit 5-level trust system |
| Permission system | None | Capability-scoped permissions |

## What Is AMARA-Specific

- Capability advertisement and discovery protocol
- Task/result lifecycle (TASK, TASK_ACCEPT, TASK_REJECT, RESULT, RESULT_CHUNK)
- Agent scoring for task routing (hop count, latency, trust, load, capability match)
- Model backend abstraction (Ollama, llama.cpp, MLX, custom HTTP)
- Resource metadata (CPU class, memory class, GPU, battery, thermal)
- Multi-agent delegation with lineage tracking and loop prevention
- Deterministic mesh simulator for testing
- Local HTTP API for integrations
- Python/Node SDKs
- Desktop application (Tauri)
- Agent-to-agent autonomous communication

## Wire-Level Interoperability

AMARA's BLE transport adapter is designed to coexist with BitChat on shared BLE channels when technically appropriate. However:

1. AMARA uses its own service UUIDs for BLE advertisement
2. AMARA packet headers are not wire-compatible with BitChat
3. No automatic protocol bridging is implemented
4. A future bridge adapter could translate between protocols at the transport level

The decision to use separate wire formats is deliberate: AMARA's packet types, capability system, and task model have no equivalent in BitChat's messaging protocol. Pretending wire compatibility would create fragile and misleading abstractions.
