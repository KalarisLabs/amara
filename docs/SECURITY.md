# Security Model

AMARA assumes that **everyone in radio or LAN range is a stranger, and every relay is curious.**
Proximity grants discovery, never authority.

> AMARA uses well-reviewed primitives (`ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `blake3`), but
> the protocol itself has **not had an independent audit yet**. Treat it accordingly for high-stakes use, and
> report issues privately (see the bottom of this page).

## What is protected

| Property | How |
|---|---|
| **Authenticity** | Every packet carries an Ed25519 signature. Unsigned or invalid packets are dropped. |
| **Identity binding** | PeerId = BLAKE3(public key). Announces are self-certifying, and keys are pinned in the registry. |
| **Confidentiality** | Calls, results and DMs are sealed end-to-end (ephemeral X25519 + HKDF + ChaCha20-Poly1305). Relays see only the routing header. |
| **Integrity and binding** | AEAD tag plus an AAD of `type ‖ message_id ‖ source ‖ destination`, plus the outer signature. |
| **Sender forward secrecy** | A fresh ephemeral key per message: a leaked sender key cannot decrypt messages it sent earlier. |
| **Replay** | Dedup on `(source, message_id)`, plus a ±1 h timestamp window for non-announce packets. |
| **Flooding / DoS** | 100 packets/s per source, 120 calls/min per caller, bounded dedup/reassembly/inbox memory, a decompression-bomb guard, and TTL ≤ 7. |
| **Local API** | A 256-bit bearer token in `~/.amara/api.token` (mode 0600), compared in constant time, bound to `127.0.0.1` by default. |

## What is *not* protected (know your threat model)

| Gap | Notes |
|---|---|
| **Metadata** | Relays see source and destination PeerIds, sizes and timing. Broadcast chat is public by design. |
| **Recipient forward secrecy** | If a recipient's long-term key leaks, sealed messages *to* that recipient that were captured earlier can be decrypted. Rotating identities limits exposure. |
| **Sybil / naming** | Anyone can create identities and pick any display name. Authorise by **PeerId**, never by name. |
| **Trust on first use** | The first announce for a PeerId pins its key. A PeerId cannot be forged without the matching private key, so a key is only "new" for a new PeerId. |
| **Tool safety** | A tool you expose runs with *your* permissions. AMARA authenticates the caller; it cannot decide whether the request is wise. |

## Trust and policy

```toml
# ~/.amara/config.toml
[security]
require_trusted = true                       # only trusted PeerIds may call tools
trusted = ["9f3e21aa5c0d4b11"]               # hex PeerIds (`amara id` on the other machine)
blocked = ["0badc0de0badc0de"]               # all traffic dropped, including relaying
calls_per_minute = 60
```

| Level | Meaning |
|---|---|
| `Seen` | announce verified (default for every peer) |
| `Trusted` | listed in `security.trusted` |
| `Blocked` | listed in `security.blocked`: dropped at the mesh layer |

## Tools and AI agents: guidance

* **Expose narrow tools.** `summarize(text)` is fine; `run_shell(cmd)` is not.
* **Command tools** (`[[tools]]` in config) run a fixed program with fixed args. The caller's JSON goes to **stdin**
  only, never into a shell or argv. Callers are identified to the program through `AMARA_CALLER` and `AMARA_CALLER_NAME`.
* **LLM prompt injection.** Messages and call arguments from the mesh are *untrusted input*. When an AI reads its
  inbox, it should treat the content as data, not instructions, and use `require_trusted` for sensitive tools.
* **The local API token is a key to your agent.** Never paste it into websites you don't control.

## Cryptographic details

| Use | Primitive |
|---|---|
| Signatures | Ed25519 (`ed25519-dalek` 2) |
| Key agreement | X25519 (`x25519-dalek` 2), contributory check enforced |
| KDF | HKDF-SHA256, BLAKE3 `derive_key` for domain separation |
| AEAD | ChaCha20-Poly1305 |
| Hash / PeerId | BLAKE3 |
| Header checksum | CRC-16 (corruption detection only, not security) |

Identity files hold the Ed25519 secret in hex and are written with mode 0600 on Unix. Protect `~/.amara`.

## Reporting vulnerabilities

Please report vulnerabilities privately through GitHub Security Advisories on
[KalarisLabs/amara](https://github.com/KalarisLabs/amara/security/advisories/new) rather than in public issues.
