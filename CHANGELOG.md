# Changelog

## 0.2.0

The first usable release: AMARA works end to end on real networks.

### Added
- **UDP LAN transport**: zero-config multicast discovery, unicast replies, and static peers for VPNs/clouds.
- **End-to-end sealed boxes** for calls, results and DMs (ephemeral X25519 + HKDF + ChaCha20-Poly1305, header-bound AAD).
- **Self-certifying announces** carrying Ed25519 and X25519 keys; signature verification for every packet.
- **Learned next-hop routing** for unicast; jittered flooding for broadcasts; per-hop framing and fragmentation.
- **Tool calling**: Rust fn tools, command tools, AI-served inbox tools; capability anycast (`@tag` or by tool name); built-in `amara.card` and `amara.ping`.
- **`amara` binary**: `mcp`, `up` (HTTP API + SSE), `chat`, `setup` (Claude Code/Desktop, Codex, Cursor, Windsurf, VS Code, Gemini), `peers`, `send`, `call`, `card`, `inbox`, `schema`, `simulate`, `doctor`, `id`.
- **SDKs**: Python and JavaScript (zero dependencies), Rust builder (`amara-agent`).
- Trust/block lists, per-peer rate limits, reassembly memory caps, decompression-bomb guard.
- Installers, CI on Linux/macOS/Windows, release builds for five targets.

### Fixed
- Packet signatures failed after encode/decode and after any relay: the `SIGNED` flag was set after signing, and the CRC covering the TTL was not masked.
- The mesh engine never read from its transports and never verified signatures.
- `TaskConstraints::default()` allowed zero delegation depth.
- `amara-protocol` was missing `blake3` and `ed25519-dalek` dependencies; `amara-core` tests were missing `rand_chacha`.
- The CLI did not compile (it referenced non-existent `PeerInfo` fields and unexported types).
