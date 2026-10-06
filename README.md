# AMARA

**A decentralized proximity network for AI agents.**

Nearby machines discover one another wirelessly, understand what capabilities each agent exposes, exchange tasks and results, relay traffic through intermediate nodes, and continue functioning with zero internet.

> Agents should be able to meet physically.
> Connectivity should emerge from proximity rather than infrastructure.

---

## Quick Start

```bash
# Install (single binary)
amara doctor        # verify system readiness
amara               # launch interactive mode
```

```text
AMARA

● 3 agents nearby

  nova        reasoning / llama-3.2 / linux
  orion       vision / qwen-vl / mac
  sana        sensors / raspberry-pi

> ask nova to analyze this dataset
> send this image to orion
> find an agent capable of OCR
```

No accounts. No API keys. No cloud. No manual IP configuration. No Docker.

Install. Run. Discover nearby agents. Use them.

---

## Architecture

```text
┌──────────────────────────────────────────────┐
│              HUMAN / AGENT UX                │
│       CLI · TUI · GUI · API · SDK            │
├──────────────────────────────────────────────┤
│              AGENT RUNTIME                   │
│ tasks · delegation · tools · execution       │
├──────────────────────────────────────────────┤
│             AMARA PROTOCOL                   │
│ capability · task · result · sync · control  │
├──────────────────────────────────────────────┤
│             MESH ENGINE                      │
│ discovery · routing · relay · dedup · TTL    │
├──────────────────────────────────────────────┤
│             LINK TRANSPORT                   │
│ BLE · localhost simulation · future Wi-Fi    │
└──────────────────────────────────────────────┘
```

Each layer is cleanly separated. The networking layer knows nothing about LLMs. The agent layer knows nothing about BLE. The UI knows nothing about packet routing.

## Core Principles

- **Proximity-first**: Connectivity emerges from physical nearness
- **Zero infrastructure**: No central servers, DNS, or cloud dependencies
- **Cryptographic identity**: Ed25519/X25519 — never trust proximity alone
- **Offline-first**: Internet is optional, not required
- **Privacy by default**: No analytics, no telemetry, no cloud logs
- **Single binary**: One install, one executable, zero runtime dependencies

## Platform Support

| Platform | Status |
|----------|--------|
| Linux x64 | ✓ |
| Linux ARM64 | ✓ |
| macOS Apple Silicon | ✓ |
| macOS Intel | ✓ |
| Windows x64 | ✓ |
| Raspberry Pi OS | ✓ |

## Documentation

- [ARCHITECTURE.md](docs/ARCHITECTURE.md) — System design and layer boundaries
- [PROTOCOL.md](docs/PROTOCOL.md) — Wire protocol specification
- [SECURITY.md](docs/SECURITY.md) — Security model and threat analysis
- [SIMULATOR.md](docs/SIMULATOR.md) — Deterministic mesh simulator
- [SDK.md](docs/SDK.md) — Agent development guide
- [INSTALL.md](docs/INSTALL.md) — Installation instructions
- [DEVELOPMENT.md](docs/DEVELOPMENT.md) — Development setup
- [CONTRIBUTING.md](docs/CONTRIBUTING.md) — Contribution guidelines

## License

MIT OR Apache-2.0
