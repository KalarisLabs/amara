# AMARA Protocol Specification

**Version**: 1.0-draft
**Date**: 2026-10-06

## 1. Overview

The AMARA protocol is a compact binary protocol designed for bandwidth-constrained proximity networks (BLE, etc.). It supports discovery, capability exchange, task delegation, result delivery, encrypted communication, and mesh relay.

## 2. Encoding

All multi-byte integers are **big-endian** (network byte order).
All strings are UTF-8.
Variable-length fields are length-prefixed with a 2-byte (u16) length.

## 3. Common Packet Header

Every AMARA packet begins with a 42-byte fixed header:

```text
Offset  Size  Field           Description
──────  ────  ─────           ───────────
0       1     version         Protocol version (currently 0x01)
1       1     packet_type     Packet type identifier
2       1     flags           Bit flags (see §3.1)
3       1     ttl             Time-to-live (max 7, decremented per hop)
4       8     message_id      Unique message identifier (random)
12      8     source_id       Sender's PeerId (8 bytes)
20      8     destination_id  Recipient PeerId (0x00..00 for broadcast)
28      4     timestamp       Unix timestamp (seconds, u32)
32      4     payload_length  Payload size in bytes (u32, max 16 MiB)
36      2     sequence        Sequence number for ordering
38      2     reserved        Reserved for future use (must be 0x0000)
40      2     checksum        CRC-16 of header bytes [0..40)
```

Total: 42 bytes

### 3.1 Flags

```text
Bit  Name            Description
───  ────            ───────────
0    ENCRYPTED       Payload is encrypted to destination
1    FRAGMENTED      This is a fragment (see §7)
2    RELAY_ELIGIBLE  May be relayed by intermediate nodes
3    ACK_REQUESTED   Sender wants an ACK
4    COMPRESSED      Payload is LZ4-compressed
5    SIGNED          Ed25519 signature appended after payload
6    PRIORITY        High-priority packet
7    RESERVED        Must be 0
```

### 3.2 Signature Trailer

When SIGNED flag is set, a 64-byte Ed25519 signature follows the payload. The signature covers the entire packet excluding TTL byte (offset 3) and the signature itself, allowing relays to decrement TTL without invalidating the signature.

```text
[header (42)] [payload (N)] [signature (64)]
```

## 4. Packet Types

### 4.1 Type Table

```text
ID    Name              Direction     Description
──    ────              ─────────     ───────────
0x01  ANNOUNCE          broadcast     Periodic presence announcement
0x02  CAPABILITY        broadcast     Capability advertisement
0x03  PING              direct        Liveness check
0x04  PONG              direct        Liveness response

0x10  TASK              direct        Task request
0x11  TASK_ACCEPT       direct        Task accepted
0x12  TASK_REJECT       direct        Task rejected
0x13  TASK_CANCEL       direct        Cancel running task

0x20  RESULT            direct        Complete result
0x21  RESULT_CHUNK      direct        Partial result chunk
0x22  RESULT_COMPLETE   direct        Final result marker

0x30  EVENT             broadcast     General event notification
0x31  HEARTBEAT         broadcast     Periodic keepalive

0x40  ACK               direct        Acknowledgement
0x41  ERROR             direct        Error response

0x50  ROUTE_REQUEST     broadcast     Route discovery
0x51  ROUTE_RESPONSE    direct        Route reply

0x60  STORE             direct        Store-and-forward request
0x61  FORWARD           direct        Forwarded stored message

0x70  FILE_MANIFEST     direct        File transfer metadata
0x71  FILE_CHUNK        direct        File data chunk
0x72  FILE_COMPLETE     direct        File transfer complete

0x80  KEY_EXCHANGE      direct        Key agreement initiation
0x81  ENCRYPTED         direct        Encrypted envelope

0xF0-0xFF  RESERVED     —             Reserved for future versions
```

### 4.2 ANNOUNCE Payload

```text
Offset  Size  Field
0       2     name_length
2       N     display_name (UTF-8)
N+2     2     version_length
N+4     M     version_string
M+N+4   1     platform_id (0=unknown, 1=linux, 2=macos, 3=windows, 4=rpi)
M+N+5   1     capability_count
M+N+6   ...   capability_ids (1 byte each)
...     1     load_class (0=idle, 1=low, 2=medium, 3=high, 4=overloaded)
...     1     availability (0=offline, 1=online, 2=busy, 3=away)
```

### 4.3 CAPABILITY Payload

```text
Offset  Size  Field
0       2     json_length
2       N     capability_json (compact JSON document)
N+2     4     cap_version (monotonically increasing)
N+6     8     cap_hash (BLAKE3 hash of capability_json, truncated to 8 bytes)
```

The capability JSON follows the schema defined in ARCHITECTURE.md §7.

### 4.4 TASK Payload

```text
Offset  Size  Field
0       16    task_id (UUID, 16 bytes)
16      16    parent_task_id (all zeros if no parent)
32      1     capability_id
33      1     priority (0-10, 5=normal)
34      4     deadline (Unix timestamp, 0=no deadline)
38      2     input_length
40      N     input_data (MessagePack or JSON depending on COMPRESSED flag)
```

### 4.5 RESULT Payload

```text
Offset  Size  Field
0       16    task_id (UUID matching the request)
16      1     status (0=success, 1=error, 2=partial, 3=timeout)
17      4     result_length
21      N     result_data
```

## 5. Deduplication

Every packet carries an 8-byte `message_id`. Nodes maintain a bounded set of recently seen IDs (default capacity: 10,000). Duplicate packets are silently dropped.

The dedup set is an LRU cache keyed by `(source_id, message_id)`.

## 6. TTL and Relay

- Default TTL: 7
- Each relay decrements TTL by 1
- Packets with TTL=0 are dropped (never relayed)
- Relay jitter: 10-100ms random delay before forwarding
- Split horizon: never relay back to the transport/peer that delivered the packet
- Packets without RELAY_ELIGIBLE flag are never relayed

## 7. Fragmentation

When a message exceeds the transport MTU (default 500 bytes for BLE), the protocol layer fragments it.

### Fragment Header (appended to standard header)

```text
Offset  Size  Field
0       8     fragment_group_id (same as parent message_id)
8       2     fragment_index (0-based)
10      2     total_fragments
12      2     fragment_payload_length
14      N     fragment_payload
```

### Reassembly Rules

- Maximum fragments per message: 256
- Maximum total reassembled size: 16 MiB
- Reassembly timeout: 30 seconds
- Maximum concurrent reassemblies: 64
- Duplicate fragments: silently ignored
- Out-of-order: supported (index-based)
- Missing fragments after timeout: assembly discarded

## 8. Encryption

### 8.1 Key Exchange

AMARA uses a Noise NK handshake pattern for establishing session keys when the initiator knows the responder's static public key (from ANNOUNCE):

```text
Initiator → Responder:  KEY_EXCHANGE { ephemeral_public, encrypted_payload }
Responder → Initiator:  KEY_EXCHANGE { ephemeral_public, encrypted_payload }
```

After handshake, both parties derive symmetric keys for authenticated encryption (ChaCha20-Poly1305).

### 8.2 Encrypted Packets

ENCRYPTED packets wrap an inner packet:

```text
Offset  Size  Field
0       8     session_id
8       12    nonce
20      N     ciphertext (ChaCha20-Poly1305 AEAD)
20+N    16    auth_tag
```

Relay nodes see only the outer header (source, destination, TTL) — the inner payload is opaque.

## 9. Protocol Versioning

- `version` field in header allows future protocol evolution
- Unknown packet types from same major version: silently ignored
- Higher major version: connection rejected with ERROR
- Feature flags may be added via reserved header bits
- All optional fields must be safely ignorable

## 10. Error Codes

```text
Code  Name                Description
0x01  UNKNOWN_PEER        Peer not found
0x02  UNSUPPORTED_VERSION Protocol version mismatch
0x03  INVALID_SIGNATURE   Signature verification failed
0x04  EXPIRED             Packet or task expired
0x05  RATE_LIMITED         Too many requests
0x06  CAPABILITY_MISSING  Requested capability not available
0x07  TASK_NOT_FOUND      Referenced task doesn't exist
0x08  PERMISSION_DENIED   Trust/policy check failed
0x09  OVERLOADED          Agent is at capacity
0x0A  FRAGMENT_ERROR      Reassembly error
0x0B  SIZE_EXCEEDED       Payload too large
0x0C  MALFORMED           Packet parsing failed
```

## 11. Size Limits

| Constraint | Limit |
|-----------|-------|
| Max packet size | 16 MiB |
| Max fragment payload | 488 bytes (500 - 12 byte frag header) |
| Max fragments per message | 256 |
| Max capability JSON | 4 KiB |
| Max display name | 64 bytes |
| Max concurrent reassemblies | 64 |
| Dedup cache size | 10,000 entries |
| Max TTL | 7 |
| Max task input | 8 MiB |
| Max result data | 8 MiB |
| Max stored messages | 1,000 |
| Store-and-forward TTL | 12 hours |
