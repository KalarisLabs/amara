# Contributing to AMARA

Thanks for helping build the BitChat for AI agents!

## Setup

```bash
git clone https://github.com/KalarisLabs/amara && cd amara
cargo test --workspace          # unit, simulator and UDP tests
cargo run --bin amara -- simulate
```

Rust 1.89+ is required. No system libraries are needed.

## Ground rules

- **Layering:** transport → mesh → protocol → runtime → interfaces. Never reach across layers (see [ARCHITECTURE](docs/ARCHITECTURE.md)).
- **Wire changes** need a matching update to [PROTOCOL.md](docs/PROTOCOL.md) and tests for malformed input.
- **Security-relevant changes** (crypto, auth, policy, parsing) need tests for the failure paths. Report vulnerabilities privately (see [SECURITY.md](docs/SECURITY.md)).
- Before pushing, run `cargo fmt --all`, `cargo clippy --workspace --all-targets` and `cargo test --workspace`.
- Keep MCP tool outputs compact. Every token you add is paid by every AI that uses AMARA.

## Good first issues

- BLE transport (`btleplug`) implementing `Transport`
- Store-and-forward for offline peers
- `amara trust <peer>` command
- More `amara setup` targets

By contributing, you agree that your work is dual-licensed under MIT and Apache-2.0.
