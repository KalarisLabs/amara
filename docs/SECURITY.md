# AMARA Security Model & Threat Analysis

## Principles

1. **Proximity ≠ Trust**: Physical nearness grants discovery, not authority
2. **Cryptographic identity**: All nodes identified by Ed25519 keys
3. **End-to-end encryption**: Task payloads encrypted to recipient; relays see only routing metadata
4. **Defense in depth**: Multiple layers of protection at each system level
5. **Fail secure**: Unknown states default to deny

## Trust Levels

```text
UNKNOWN    → Newly discovered peer, no interaction
SEEN       → Exchanged announcements, verified signature
VERIFIED   → Completed key exchange, authenticated identity
TRUSTED    → Explicitly trusted by user
BLOCKED    → Explicitly blocked, all traffic dropped
```

## Permission Model

```yaml
# Default permissions for remote tasks
permissions:
  inference: allow      # Run inference on local models
  filesystem: deny      # No file system access
  shell: deny           # No shell execution
  camera: deny          # No camera access
  microphone: deny      # No microphone access
  network: deny         # No outbound network from tasks
  sensors: ask          # Prompt user for sensor access
```

## Threat Matrix

| Threat | Mitigation |
|--------|-----------|
| Packet replay | Message IDs + dedup cache + timestamp validation |
| Spoofed identity | Ed25519 signature on all packets |
| Malformed packets | Strict parser with size limits and validation |
| Oversized fragments | Fragment count limit (256), size limit per fragment |
| Flooding | Per-peer rate limiting, backpressure signaling |
| Task spam | Task quotas per peer, trust-based filtering |
| Capability spoofing | Capabilities signed by node, verifiable |
| Duplicate tasks | Task ID dedup, lineage tracking |
| Malicious agents | Sandboxed execution, permission model |
| Relay abuse | TTL, relay eligibility flag, rate limiting |
| Resource exhaustion | Bounded queues, memory limits, timeout on all operations |
| Downgrade attacks | Minimum protocol version enforcement |
| Stale peers | Expiration on registry entries, heartbeat required |
| Poisoned metadata | Validation of all received metadata, size limits |
| Task loops | Delegation depth limit, visited-agent tracking, budget |
| Fragment bomb | Max concurrent reassemblies, timeout, total size limit |
| Store-forward abuse | Quota per peer, expiration, encrypted-only storage |

## Cryptographic Primitives

| Purpose | Algorithm | Library |
|---------|-----------|---------|
| Signing | Ed25519 | ed25519-dalek |
| Key agreement | X25519 | x25519-dalek |
| Hashing | BLAKE3 | blake3 |
| AEAD encryption | ChaCha20-Poly1305 | chacha20poly1305 |
| Key derivation | HKDF-SHA256 | hkdf |
| Random | OS CSPRNG | rand |
| Handshake | Noise NK | snow |

## Packet Signature

All outgoing packets are signed with the sender's Ed25519 key. The signature covers the entire packet excluding:
- TTL byte (offset 3) — allows relay nodes to decrement
- The signature itself (appended after payload)

Receivers verify the signature against the sender's known public key. Packets with invalid signatures are dropped and logged as security events.

## Rate Limiting

Default limits per peer:
- 100 packets/second
- 10 tasks/minute
- 50 MiB/hour transfer
- 5 key exchange attempts/minute

These are configurable but conservative defaults prevent abuse.

## Sandboxing

Remote tasks execute in a restricted runtime:
- No direct filesystem access (only task-provided inputs)
- No shell execution
- No network access from task context
- Scoped to declared capability
- Memory and CPU time limits
- Result size limits
