# Architecture

How atmospheric-horizon is put together, for reviewers, contributors, and coding agents. Start with `AGENTS.md` for the working rules.

## The two layers

```text
┌──────────────────────────────┐   GameCommand channel   ┌──────────────────────────┐
│ Horizon game thread          │ ───────────────────────▶ │ worker thread            │
│ ┌────────────────────────┐   │                          │ tokio runtime            │
│ │ mixins/atmospheric/    │   │◀─────────────────────── │ ATrium agent / reqwest   │
│ │ runtime.rs (std-only)  │   │   ModEvent channel      │ ⇄ the PDS you configure   │
│ │ hooks + channels       │   │                          │ storage/ (session,       │
│ └────────────────────────┘   │                          │ cursor, config)          │
└──────────────────────────────┘                          └──────────────────────────┘
```

### The injected module: `mixins/atmospheric/runtime.rs`

This exact source is injected by horizon-mixins into the game as
`crate::game::atmospheric`, and it is also the standalone Cargo library of the
root package. Both builds must work, so it stays **std-only**: no external
crates, no game imports. Its only job is channel plumbing:

- `install()` creates the `GameCommand`/`ModEvent` channel pair, keeps the
  game side in statics, and hands the worker side to whoever spawns the
  worker at startup.
- Hook helpers (like the placeholder `before_break`) push `GameCommand`s
  through the sender — never blocking, never panicking if the worker is gone.
- `drain_events()` is called once per tick to collect worker events for the
  HUD, bounded so the game thread never stalls.

### The protocol library: `crates/atmospheric`

Everything else, with zero Horizon imports so it builds and tests with plain
`cargo test`:

- `config` — settings, stored under the mod's own folder
- `session` — `AtpAgent` construction and app-password login
- `actions` — `create_record` posts; `uploadBlob` arrives in M3
- `notifications` — polling with a persisted seen-cursor (dedup across
  restarts, the pattern bsky-bot-sdk uses)
- `worker` — owns the tokio runtime, the agent, and the files; the **only**
  module that performs network or disk I/O
- `error` — one error enum, mapped to HUD strings at the edge

### The wiring question for M2

The injected module lives inside the game's crate, which does not depend on
the `atmospheric` crate. The worker therefore consumes the game-side channels
and owns an `atmospheric`-crate agent inside its own thread — the two crates
never import each other, they meet only at the channel boundary. If the
native build turns out to support adding workspace dependencies to the game
crate, the boundary stays the same; only where the types live may move.

## Build and validation

- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --locked` run on every push (`.github/workflows/ci.yml`).
- Native staging: `cargo fetch --locked`, then `cargo test --workspace --locked
  --offline`, matching the template's standalone checks. Full native checks
  need the pinned Horizon export from `compatibility.json`.
- The bundle pins the exact Horizon and horizon-mixins revisions; other
  revisions have not been validated.

## Reviewer notes

- Network use is declared, not hidden: this README, the mod page, and the
  first-run UX all say what the mod talks to and what it sends.
- No Minecraft assets, code, or data are embedded; nothing is read from the
  player's files beyond the mod's own storage folder.
- Licence: AGPL-3.0-only, matching this repository's `LICENSE`.
