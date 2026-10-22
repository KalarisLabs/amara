# AMARA Wire Protocol

**Version** 1 (`0x01`) · **Status** implemented in `amara-protocol` / `amara-mesh` / `amara-runtime`

AMARA is a compact binary protocol for AI agents that meet over proximity links (LAN today, BLE next).
It covers discovery, authenticated flooding, learned routing, end-to-end encrypted tool calls, and messaging.

```text
┌───────────────────────────── one hop ─────────────────────────────┐
│ link datagram   "AM" 0x01 · link_sender(8) · frame                 │  §8  (UDP)
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
| `0x32` | `MESSAGE` | sealed `Chat` (DM) or plain `Chat` (broadcast) | either | ✓ |

Codes `0x02–0x04`, `0x11–0x13`, `0x21–0x22`, `0x30–0x31`, `0x40–0x41`, `0x50–0x51`, `0x60–0x61`, `0x70–0x72`, `0x80–0x81`
are reserved for capability documents, streaming results, acks, store-and-forward, file transfer and sessions.
Receivers ignore reserved types they do not implement.

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
// TASK — call a tool
{ "id": "uuid", "tool": "summarize", "args": { "text": "…" }, "timeout_ms": 60000 }

// RESULT — reply to a call (ok=false → data = {"error": "…"})
{ "id": "uuid", "ok": true, "data": { … }, "ms": 42 }

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

## 11. Limits

| Limit | Value |
|---|---|
| Max TTL | 7 |
| Max payload | 16 MiB (bodies are effectively capped by fragments × MTU ≈ 11 MiB on UDP) |
| Announce | 64 capabilities × 64 bytes |
| Dedup cache | 10 000 entries |
| Packets per source | 100 /s |
| Calls per caller | 120 /min (configurable) |
| Clock skew | ±3600 s |
| Peer expiry | 300 s |
