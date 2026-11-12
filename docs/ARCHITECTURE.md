# Architecture

How atmospheric-horizon is put together, for reviewers, contributors, and coding agents. Start with `AGENTS.md` for the working rules.

## The two layers

```text
┌──────────────────────────────┐   GameCommand channel   ┌──────────────────────────┐
│ Horizon game thread          │ ───────────────────────▶ │ worker thread            │
│ ┌────────────────────────┐   │                          │ tokio runtime            │
│ │ mixins/atmospheric/    │   │◀─────────────────────── │ ATrium agent / reqwest   │
│ │ runtime.rs (std-only)  │   │   ModEvent channel      │ ⇄ the PDS you configure   │
│ │ hooks + channels       │   │                          │ <data dir>/config.json   │
│ └────────────────────────┘   │                          │ (session, cursor in M3+) │
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
- Hook helpers push `GameCommand`s through the sender — never blocking,
  never panicking if the worker is gone. M2 hooks Horizon's named native
  hook points (`native-hooks.json` at the pinned revision): `mods` (the
  Options > Mods row and its keybind), `key` (the keybind posts the
  player's location), `command` (`/atmospheric login`,
  `/atmospheric post <text>`), `tick` (drains events once per tick on the
  window's game) and `hud` (draws the status line in the bottom-left
  corner).
- `drain_events()` is called once per tick to collect worker events, folding
  each into the HUD status line (`status_line()`), bounded so the game
  thread never stalls.

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

### The M2 wiring, and why the worker is not linked into the game yet

The injected module lives inside the game's crate, which does not depend on
the `atmospheric` crate. The worker therefore consumes the game-side channels
and owns an `atmospheric`-crate agent inside its own thread — the two crates
never import each other, they meet only at the channel boundary, and commands
cross it as plain data (`GameCommand`/`ModEvent` mirror `Command`/`Event`).

horizon-mixins 0.6.0 can stage a bundle's own crate into the game workspace
(`[[crate]]`), but its registry dependencies are taken only from Horizon's
existing `Cargo.lock` — the allow list — and the pinned Horizon revision's
lock has no `atrium-api`/`reqwest` (it does have `serde`, `tokio`,
`thiserror`). So the bundle stays mixins-only for now: the game-side hooks
above are real and compiled in, but no in-game consumer is attached to the
channel yet. When Horizon's lock (or the launcher) allows it, a small glue
helper calls `install()`, spawns `atmospheric::worker::spawn` with
`native_data_dir("atmospheric")` as the storage folder, and maps
`Command`↔`GameCommand`; the boundary stays exactly as it is.

Until then, M2's end-to-end proof runs the worker standalone against a real
PDS — see "Manual check" below.

### What the worker does with each command (M2)

- `Login` — reads `<storage dir>/config.json` (the only file the mod writes;
  never a world save), calls `session::login`, then
  `com.atproto.server.getSession` to learn the DID, caches agent + DID, and
  emits `Event::Session(LoggedIn { did })` or `Session(Failed { .. })`.
- `SaveConfig` — writes the settings file (creating the storage folder).
- `PostText` — `actions::post_text`, emitting `Event::Posted { uri }` or
  `Event::Failed`; without a session it fails with "not logged in" and never
  touches the network.
- `PostImage` / `RefreshNotifications` — answer `Failed` with the milestone
  that owns them (M3 / M4) instead of panicking the thread.
- `Shutdown`, or the game side dropping the command channel, stops the
  thread cleanly (the blocking channel is bridged into the tokio runtime so
  the loop can later `select!` a notification timer, M4).

### Manual check against a real PDS

`crates/atmospheric/examples/pds_check.rs` spawns the worker exactly as the
glue will, with a throwaway settings folder in the system temp dir, and
prints every event:

```sh
ATMOSPHERIC_PDS_URL="https://bsky.social" \
ATMOSPHERIC_IDENTIFIER="you.bsky.social" \
ATMOSPHERIC_APP_PASSWORD="xxxx-xxxx-xxxx-xxxx" \
ATMOSPHERIC_POST_TEXT="hello from atmospheric-horizon (M2 check)" \
cargo run -p atmospheric --example pds_check
```

Credentials come from the environment only; the example writes them to the
temp settings file the worker reads and nothing else. Expected output: a
`Session(LoggedIn { did })` event, then `Posted { uri }`, then a clean
shutdown. The offline tests in `worker.rs` cover the same paths against a
stub PDS on loopback (`cargo test --workspace --locked --offline`).

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
