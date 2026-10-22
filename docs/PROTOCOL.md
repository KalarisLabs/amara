# AMARA Wire Protocol

**Version** 1 (`0x01`) · **Status** implemented in `amara-protocol` / `amara-mesh` / `amara-runtime`

AMARA is a compact binary protocol for AI agents that meet over proximity links (LAN and Bluetooth LE).
It covers discovery, authenticated flooding, learned routing, end-to-end encrypted tool calls (including streaming
results), messaging, store-and-forward for peers that are out of range, and file transfer.

```text
┌───────────────────────────── one hop ─────────────────────────────┐
│ link datagram   "AM" 0x01 · link_sender(8) · frame                 │  §8  (UDP)
│                 "AM" 0x02 · link_sender(8) · link_dest(8) · frame  │  §8.1 (BLE)
│ frame           0xA7 · kind · packet | fragment                    │  §7
│ packet          header(42) · payload · signature(64)               │  §2–3
│ payload         Announce | sealed(JSON body) | JSON body           │  §4–6
└────────────────────────────────────────────────────────────────────┘
```

All integers are **big-endian** unless stated. Strings are UTF-8.

---

## 1. Identity

| Item | Definition |
|---|---|
| Signing key | Ed25519 key pair, generated on first run (`~/.amara/identities/<name>.json`) |
| Encryption key | X25519 secret = `BLAKE3.derive_key("amara-x25519-derivation", ed25519_secret)` |
| **PeerId** | first 8 bytes of `BLAKE3(ed25519_public_key)`, shown as 16 hex chars |
| Broadcast | PeerId `0000000000000000` |

Display names are labels, not identities. Only PeerIds are authenticated.

## 2. Header (42 bytes)

```text
Off Size Field           Notes
0   1    version         0x01
1   1    packet_type     §3
2   1    flags           §2.1
3   1    ttl             hops remaining, decremented by each relay (max 7)
4   8    message_id      random; (source_id, message_id) is the dedup key
12  8    source_id       originator PeerId
20  8    destination_id  recipient PeerId, or broadcast
28  4    timestamp       unix seconds
32  4    payload_length  bytes of payload (≤ 16 MiB)
36  2    sequence        0 (reserved for streaming)
38  2    reserved        0
40  2    checksum        CRC-16/IBM-SDLC over bytes [0..40)
```

### 2.1 Flags

| Bit | Name | Meaning |
|---|---|---|
| 0 | `ENCRYPTED` | payload is a sealed box (§5) |
| 1 | `FRAGMENTED` | reserved (fragmentation happens per hop, §7) |
| 2 | `RELAY_ELIGIBLE` | relays may forward this packet |
| 3 | `ACK_REQUESTED` | reserved |
| 4 | `COMPRESSED` | body is LZ4 (`lz4_flex` size-prepended) **before** sealing |
| 5 | `SIGNED` | 64-byte Ed25519 signature follows the payload; **mandatory** |
| 6 | `PRIORITY` | reserved |

### 2.2 Signature

```text
signature = Ed25519(header ‖ payload) with header[3] (ttl) = 0 and header[40..42] (checksum) = 0
```

TTL and the checksum (which covers the TTL) are the only bytes a relay changes, so they are masked.
The `SIGNED` flag and `payload_length` are set **before** signing. Unsigned packets are dropped.

## 3. Packet types

| Code | Name | Body | Addressing | Relay |
|---|---|---|---|---|
| `0x01` | `ANNOUNCE` | Announce (§4) | broadcast | ✓ |
| `0x10` | `TASK` | sealed `Call` | unicast | ✓ |
| `0x20` | `RESULT` | sealed `Reply` | unicast | ✓ |
| `0x21` | `RESULT_CHUNK` | sealed `Chunk` (§10.1) | unicast | ✓ |
| `0x32` | `MESSAGE` | sealed `Chat` (DM) or plain `Chat` (broadcast) | either | ✓ |
| `0x60` | `STORE` | Envelope (§11) | broadcast | ✓ |
| `0x61` | `FORWARD` | Envelope (§11) | unicast | ✓ |
| `0x70` | `FILE_MANIFEST` | sealed `FileOffer` (§12) | unicast | ✓ |
| `0x71` | `FILE_CHUNK` | sealed binary chunk (§12) | unicast | ✓ |
| `0x72` | `FILE_COMPLETE` | sealed `FileStatus` (§12) | unicast | ✓ |

Codes `0x02–0x04`, `0x11–0x13`, `0x22`, `0x30–0x31`, `0x40–0x41`, `0x50–0x51`, `0x80–0x81` are reserved for
capability documents, acks, routing and sessions. Receivers ignore reserved types they do not implement.

## 4. ANNOUNCE (presence beacon)

```text
ed25519_public (32) · x25519_public (32) · name · version · platform u8 · load u8 · availability u8 · count u8 · capability × count
```

Strings (`name`, `version`, each capability) are `len u8 · bytes`, max 64 bytes; at most 64 capabilities.

* **Self-certifying**: receivers check `BLAKE3(ed25519_public)[..8] == source_id` and verify the signature with the
  embedded key, so no prior contact is needed. The keys are then pinned in the peer registry.
* Capabilities = capability tags (`vision`, `reasoning`, …) **and** names of the tools the agent serves.
  Full tool schemas are fetched lazily with the built-in `amara.card` tool.
* Sent at startup, every 15 s, immediately when a new link neighbor or peer appears, and when tools change.
* Peers silent for 5 minutes are expired.

| platform | 0 unknown · 1 linux · 2 macos · 3 windows · 4 raspberry-pi · 5 android · 6 ios |
|---|---|
| load | 0 idle · 1 low · 2 medium · 3 high · 4 overloaded |
| availability | 0 offline · 1 online · 2 busy · 3 away |

## 5. Sealed boxes (end-to-end encryption)

Every unicast `TASK`, `RESULT` and `MESSAGE` payload is sealed to the recipient's X25519 key:

```text
e          = fresh X25519 key pair (per message)
shared     = X25519(e.secret, recipient_x25519)
key        = HKDF-SHA256(ikm = shared, salt = ∅, info = "amara-seal-v1" ‖ e.public ‖ recipient_x25519)
aad        = packet_type (1) ‖ message_id (8) ‖ source_id (8) ‖ destination_id (8)
payload    = e.public (32) ‖ ChaCha20-Poly1305(key, nonce = 0¹², body, aad)
```

* The key is unique per message, so the zero nonce is never reused.
* The AAD binds the ciphertext to its header: a relay cannot replay it under another id, sender or recipient.
* The sender is authenticated by the outer Ed25519 signature over the ciphertext.
* Receivers **reject** unencrypted unicast payloads.
* Overhead: 48 bytes.

## 6. Bodies (JSON)

Bodies are JSON so any language can speak AMARA, then LZ4-compressed when larger than 100 bytes and smaller after
compression (`COMPRESSED` flag). Decompressors refuse declared sizes above 16 MiB.

```jsonc
// TASK — call a tool (stream: true → partial output arrives as RESULT_CHUNK first)
{ "id": "uuid", "tool": "summarize", "args": { "text": "…" }, "timeout_ms": 60000, "stream": true }

// RESULT — reply to a call (ok=false → data = {"error": "…"}; chunks = RESULT_CHUNKs sent before it)
{ "id": "uuid", "ok": true, "data": { … }, "ms": 42, "chunks": 3 }

// RESULT_CHUNK — one piece of partial output
{ "id": "uuid", "seq": 0, "data": "first line" }

// MESSAGE — chat; channel only on broadcasts
{ "text": "hello mesh", "channel": "#lab" }
```

### 6.1 Built-in tools

| Tool | Returns |
|---|---|
| `amara.card` | `{name, id, version, platform, tags, tools: [{name, description, input_schema}]}` |
| `amara.ping` | `{"pong": unix_seconds}` |

The `amara.` prefix is reserved. User tool names match `^[a-z0-9_.-]{1,64}$`.

## 7. Framing and fragmentation (per hop)

```text
frame = 0xA7 · 0x00 · packet                               whole packet
frame = 0xA7 · 0x01 · group_id(8) · index u16 · total u16 · chunk   fragment
```

Each hop splits packets that exceed its link MTU and reassembles them before processing, so signatures, dedup
and routing always see whole packets, and every link can use its own MTU (UDP 1400 B, BLE ≈ 500 B).

Reassembly limits: 8192 fragments per packet, 64 concurrent groups, 64 MiB buffered, 30 s timeout.

## 8. LAN transport (UDP)

| Item | Value |
|---|---|
| Discovery / broadcast | multicast `239.77.65.82:7433`, IP TTL 1 (never leaves the subnet) |
| Unicast | each node's ephemeral UDP socket; learned from the source address of any datagram |
| Datagram | `"AM" 0x01 · link_sender_peer_id (8) · frame` |
| Static peers | optional `host:port` list, used where multicast is unavailable (VPNs, clouds) |

`link_sender` is the *previous hop*, used only to address the next datagram; it is not trusted for anything else.

### 8.1 Bluetooth LE transport

Every node runs both GATT roles, like BitChat: it advertises the AMARA service **and** scans for and connects to
other nodes advertising it.

| Item | Value |
|---|---|
| Service | `414d4152-4100-4d65-7368-000000000001` |
| RX characteristic (central → peripheral) | `…0002`, write / write without response |
| TX characteristic (peripheral → centrals) | `…0003`, notify |
| Datagram | `"AM" 0x02 · link_sender (8) · link_dest (8) · frame`, one per write or notification |
| Write size | 182 bytes by default (ATT MTU 185), so the per-hop frame MTU is 163 bytes |

A peripheral's notification reaches every subscribed central, so receivers drop datagrams whose `link_dest` is
neither their own PeerId nor broadcast (all zeros). Packets above the frame MTU are fragmented per hop (§7), which
is what lets the same signed packets ride UDP and BLE unchanged and be relayed between the two.

## 9. Mesh processing

For every reassembled packet:

1. Decode the header; drop bad version, type, CRC or size.
2. Drop our own packets and packets from or via blocked PeerIds.
3. **Dedup** on `(source_id, message_id)` with a 10 000-entry LRU.
4. **Rate-limit** at 100 packets/s per source.
5. **Authenticate**: ANNOUNCE is self-certifying (§4); anything else needs the key pinned from the source's
   announce and a timestamp within ±1 h.
6. **Deliver** if the packet is for us or broadcast.
7. **Relay** if relaying is enabled, `RELAY_ELIGIBLE`, `ttl > 0`, and the packet is not addressed to us:
   * broadcast → re-flood after 10–100 ms of random jitter (storm suppression);
   * unicast → send to the learned next hop for the destination; if there is no route, flood.

**Routes** come from announces: the neighbor that delivered the copy with the fewest hops
(`hops = 7 − ttl + 1`) becomes the next hop. Routes are refreshed by every announce, replaced when a shorter path
appears or the current one goes stale (45 s), and dropped when a link neighbor is lost.

## 10. Calls

```text
caller                         relay(s)                       callee
  │ TASK sealed(Call) ───────────►│──────────────────────────────►│ policy: blocked? trusted-only? rate (120/min)?
  │                               │                               │ run tool (≤ timeout_ms, max 300 s)
  │◄──────────────────────────────│◄────────────── RESULT sealed(Reply)
```

* Caller timeout defaults to 60 s.
* **Target resolution**: an explicit PeerId or name; `@tag` → best peer advertising the tag; none → best peer
  advertising a tool with that name. Best = fewest hops, then lowest load, then highest trust.
* **Inbox tools**: an agent may expose a tool that an external AI answers (MCP `expose` → `inbox` → `reply`).
  If nothing answers within the caller's timeout, the caller receives a timeout.

### 10.1 Streaming results

A caller sets `"stream": true` on the `Call`. The callee then sends partial output as sealed `RESULT_CHUNK` packets
(`seq` 0, 1, 2, …) before the final `RESULT`, whose `chunks` field says how many were sent. Chunks may overtake each
other or the result on a multi-path mesh, so the caller re-orders by `seq` and waits up to 2 s after the result for
stragglers. Chunks from any PeerId other than the callee are ignored.

Producers: Rust tools write to `ctx.stream`; command tools stream each stdout line; inbox-served tools answer with
`reply(partial: true)` before the final `reply`. A tool that is called without `stream` produces no chunks.

## 11. Store and forward

A directed packet for a peer that is out of range (not in the registry) but whose keys are known is held instead of
dropped. Keys of every verified peer are remembered (up to 4096, persisted in `state_dir`).

```text
envelope = sender_ed25519 (32) · sender_x25519 (32) · name (u8 len · bytes) · original signed packet
```

1. The sender signs the original packet (e.g. a sealed `MESSAGE`), keeps the envelope in its own custody, and
   broadcasts it in a `STORE` packet.
2. Every node with custody enabled (the default) that hears the `STORE` verifies it: the inner packet must be signed
   by the envelope's key, that key must hash to the inner `source_id`, and the inner source must equal the `STORE`'s
   source. It then holds the envelope for the inner destination. If it can see the destination itself, it forwards
   immediately.
3. When the destination announces after being absent (new, or silent for 45 s or more), every custodian sends it
   the envelope in a `FORWARD` packet addressed to it.
4. The destination verifies the inner packet against the envelope's keys (self-certifying, so the sender need not be
   online), dedups on the inner `(source_id, message_id)` so copies from several custodians collapse into one, and
   delivers it flagged `stored`. The inner timestamp may be up to 12 h + 1 h old.

The payload stays sealed end to end: custodians learn only sender, recipient, size and time.

| Limit | Value |
|---|---|
| Held packets | 1000 per node, 100 per destination (oldest dropped first) |
| Lifetime | 12 hours |
| Eligible | directed `MESSAGE` (DMs). Calls and files need both ends in range. |

## 12. File transfer

```text
sender                                        receiver
  │ FILE_MANIFEST sealed(FileOffer) ──────────►│ policy: accept? size? trusted?
  │◄────────────── FILE_COMPLETE {accepted | rejected}
  │ FILE_CHUNK sealed(id · index · bytes) × n ─►│ written straight to disk
  │ FILE_COMPLETE {sent} ──────────────────────►│
  │◄────────────── FILE_COMPLETE {missing: [..]}  resend those, ask again (≤ 10 rounds)
  │◄────────────── FILE_COMPLETE {received}       BLAKE3 verified, file saved
```

```jsonc
// FILE_MANIFEST
{ "id": "uuid", "name": "report.pdf", "size": 1048576, "chunk": 32768, "hash": "<hex BLAKE3>" }
// FILE_COMPLETE (either direction)
{ "id": "uuid", "state": "accepted|rejected|sent|missing|received", "missing": [3, 9], "error": "…" }
```

`FILE_CHUNK` is the only binary body: `id (16) · index u32 · bytes`, sealed like everything else. Chunks are 32 KiB
and paced at 50 per second, half of every relay's per-source budget. Receivers sanitize the name (no directories,
no hidden files) and never overwrite: `name (1).ext`. Defaults: files from anyone up to 32 MiB, at most 8 transfers
in flight, stalled transfers dropped after 120 s.

## 13. Limits

| Limit | Value |
|---|---|
| Max TTL | 7 |
| Max payload | 16 MiB (bodies are effectively capped by fragments × MTU ≈ 11 MiB on UDP, ≈ 1.3 MiB on BLE) |
| File size | 32 MiB by default (`files.max_mb`), any size in principle (32 KiB chunks) |
| Store and forward | 1000 packets, 100 per destination, 12 h |
| Announce | 64 capabilities × 64 bytes |
| Dedup cache | 10 000 entries |
| Packets per source | 100 /s |
| Calls per caller | 120 /min (configurable) |
| Clock skew | ±3600 s |
| Peer expiry | 300 s |
