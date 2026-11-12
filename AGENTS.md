# AGENTS.md

Guidance for AI coding agents working in **atmospheric-horizon**.

## Project overview

- Language: Rust (edition 2024, MSRV 1.95, pinned by `rust-toolchain.toml`)
- What: a native Horizon mod (`.hnmod`, via horizon-mixins) that bridges Horizon — the Rust rewrite of Minecraft: Java Edition — to AT Protocol / Bluesky
- Protocol layer: [ATrium](https://github.com/atrium-rs/atrium) (`atrium-api`, `atrium-xrpc-client`)
- Default branch: main

## Architecture rules

- Two layers, one boundary:
  - `mixins/atmospheric/runtime.rs` — the injected module (`crate::game::atmospheric`) **and** the standalone Cargo library of the root package. It must stay **std-only**: no external crates, no game imports. It owns the command/event channels only.
  - `crates/atmospheric` — all AT Protocol logic, builds and tests without a game. The worker thread (`worker.rs`) is the **only** place that performs network or disk I/O.
- All AST changes to Horizon sources go through horizon-mixins plan/apply; never hand-edit generated hooks or injected copies, and never add string-based Rust source rewriting.
- Keep this repository separate from Horizon and horizon-mixins. Only export committed pinned Horizon sources into ignored target trees; never modify the original checkout or player resources. Do not commit binaries, exported sources or Minecraft assets.
- Read `compatibility.json` before editing; its pinned revisions are the only validated build targets.

## Working rules

- Inspect the README, manifests, CI workflows, and nearby code before editing.
- Preserve existing architecture, naming, formatting, and error-handling conventions.
- Use project scripts for validation; never claim checks you did not run.
- Keep changes scoped and update tests or documentation when behavior changes.
- Use feature branches and pull requests.
- Treat credentials (app passwords), session files, generated files, and release metadata as sensitive.

## Network disclosure

This mod talks to the network **by design**: the PDS the user configures, and nothing else. Any change that adds an endpoint, a field sent over the wire, or a new kind of stored data must update `README.md` (Network use) and `docs/ARCHITECTURE.md` in the same pull request.

## Validation

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

Native staging against the pinned Horizon export (when the horizon-mixins tooling is set up locally):

```bash
cargo fetch --locked
cargo test --workspace --locked --offline
```

Keep validation claims and supported revisions honest.

## Recent history

Initial scaffold.
