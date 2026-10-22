# Changelog

## 0.3.0

Agents and robots that work with no network at all, and stay in touch when they drift out of range.

### Added
- **Bluetooth LE transport** (`--ble`, feature `ble`): every node is GATT central and peripheral at once (btleplug +
  ble-peripheral-rust: BlueZ, WinRT, CoreBluetooth). Link logic sits behind a `BleRadio` trait, so phones can plug
  in their own Bluetooth stack; `BleAir` simulates radios in tests.
- **Store and forward**: DMs to out-of-range agents are sealed into a self-certifying envelope and held by the sender
  and every nearby custodian, then forwarded when the recipient returns. Known peers and held packets persist across
  restarts (`~/.amara/state/<name>`).
- **Streaming results**: `call_streaming` / `call --stream` / HTTP SSE / MCP progress notifications. Rust tools write
  to `ctx.stream`, command tools stream stdout lines, inbox tools `reply(partial: true)`.
- **File transfer**: `send-file`, `/v1/files`, MCP `send_file`. Sealed 32 KiB chunks, receiver-driven repair,
  BLAKE3 verification, sanitized names, per-node policy (`[files]`).
- **Robots**: live topics (`publish`, `topics`, `--watch`) with a latest-value cache on every node, and a signed,
  mesh-wide **emergency stop** (`amara stop`, MCP `estop`, `[robot] estop_from / estop_command`).
- **Desktop tray app** (`amara tray`, feature `tray`) and a **dashboard** at `/ui` that also works on phones
  (`amara up --lan`, `amara pair` prints a QR code).
- `amara bench`: latency and throughput of the whole stack. `amara simulate` now demos every feature.
- SDKs: `call_stream`/`callStream`, `send_file`/`sendFile`, `files`, `download`, `publish`, `topics`, `watch`,
  `estop`, partial replies.

### Changed
- Packets from peers met before are verified against their pinned keys even between beacons, so a returning peer is
  heard immediately.
- Per-source flood limit raised to 200 packets/s and made configurable (`packets_per_sec`).
- The simulator skips timers on zero-latency links.


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
