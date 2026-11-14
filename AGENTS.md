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
  - `crates/atmospheric` — all AT Protocol logic, builds and tests without a game. The worker thread (`worker/service.rs`) is the **only** place that performs network or disk I/O.
- Rust modules follow `<scope>/[<subscope>/]<atom>.rs`: the directory names the domain scope, the final file name is a single-word atom, and `mod.rs` only declares the scope. `crates/atmospheric`, `crates/atmospheric-server` and the mixin helper scopes are migrated; keep new code in scopes rather than adding flat files or multi-word atoms. Entry points (`lib.rs`, `main.rs`) and the injected `runtime.rs` keep their required paths.
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

## Git history and pull requests (hard rules)

- **Atomic, scoped commits.** One logical change per commit, with a subject that says what and why (`type: what — detail`). No WIP, fixup, or "misc" commits on `main`; each commit should leave the tree buildable and tests passing where feasible. Split large work into a sequence of atomic commits on the PR branch.
- **Rebase PRs, never merge commits into them.** Keep a PR branch linear: `git rebase` onto the target branch (resolving conflicts as they come) and push the branch with `--force-with-lease`. Do not `git merge main` into a feature branch.
- **Merge PRs with rebase only.** `gh pr merge <n> --rebase`. Squash merges and merge commits are not allowed on `main` — they destroy the atomic commits. Branch protection on `main` enforces linear history (`required_linear_history`) plus the `check` status check; do not weaken it.
- **History rewrites of `main` are exceptional.** They require explicit owner authorization in the PR/issue first. Because `main` forbids force-pushes, a rewrite needs a temporary, announced change to the branch protection (`allow_force_pushes: true`), a single `--force-with-lease` push of the rewritten branch, and immediate restoration of `allow_force_pushes: false`.

## Network disclosure

This mod talks to the network **by design**: the PDS the user configures, the public AppView and labelers, and the optional identity service configured for link claims, browser link creation/OAuth and join-time profile checks. Any change that adds an endpoint, a field sent over the wire, or a new kind of stored data must update `README.md` (Network use) and `docs/ARCHITECTURE.md` in the same pull request.

## Validation

```bash
bash scripts/check-layout.sh
bash scripts/test-layout.sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

The dependency-free layout check and its isolated fixture test run in CI's `check` job. The check enforces single-word Rust filenames everywhere plus scope placement and `mod.rs` declarations in both Rust crates.

Native staging against the pinned Horizon export (when the horizon-mixins tooling is set up locally):

```bash
cargo fetch --locked
cargo test --workspace --locked --offline
```

Keep validation claims and supported revisions honest.

## Recent history

M1 scaffold; M2 worker wiring (login + posts, game-side hooks); docs (successor + atpcraft roadmap); M3 screenshot posts; M4 notification polling; M5 atpcraft chat records; M6 snapshots; M8 OAuth; M9 skins; M10 server profile; M11 list reads; M12 label reads (#29 read side); M13 presence reads (#31); M14 skin reads (#33); M15 join-gate decision (#36); M16 chat-history reads (#38); M17 sidecar worker process — the game spawns `atmospheric-worker` next to the executable and bridges the channel over stdio JSON-lines (#40, issue #2 option 3); M18 v0.1.0 release — changelog, tag-driven per-OS worker archives and the mixins bundle (#42); M19 server-profile reads (#44); M20 social-graph reads (#48); M21 reply/like/repost interactions (#53); repository conventions — shared issue/PR templates, gitattributes, README sections (#50); M22 v0.2.0 release (#55); M23 social-graph writes — follow/unfollow, block/unblock and mute/unmute (#60); M24 profile reads — AppView profile plus direct record, with atpcraft's pronoun precedence (#63); M25 server account — separate persisted OAuth session, startup resume/refresh, and server-profile publishing as that account (#65); M26 Colibri bridge core — channel parsing, service-auth WebSocket stream, inbound native chat, outbound player records and OAuth grants (#67); M27 Colibri membership and token rotation — server-account membership repair plus 45-minute service-auth re-mint (#69); M28 Colibri server event notices — broadcast_message join/leave/death/advancement relay as the server account (#71); M29 Colibri presence fallback and approval notice — connection-event fallback for hidden/rewritten join/leave lines plus the community approval read (#73); M30 Colibri local profile prompt — checks the local player's actor profile at bridge start and shows atpcraft's setup link when absent (#75); M31 Colibri reconnect and send throttle — re-mints and restarts only the stream, plus atpcraft's 20-per-minute player limit (#77); M32 local Colibri player membership — repairs the player's own community record and shows the pending-approval line (#79); M33 Colibri incoming chat hygiene — atpcraft-style control/whitespace cleanup and 64/512 caps (#81); M34 Colibri community URI in bridge status (#83); M35 server-side player identity service — optional `atmospheric-server` with public UUID reads, admin-token writes, one-time pairing codes, atomic persistence and a Docker end-to-end test (#85); M36 in-game identity link claim — `/atmospheric link <code>` posts the code, player DID and optional handle to `server_api_url` and reports the linked player (#87); M37 server-wide Colibri profile checks — the join hook carries the player UUID, the worker resolves the public link and prompts a linked player with no actor profile via targeted chat (#89); M38 linked-account join gate — `player_login` waits for the worker's public link plus list decision, refuses before placement, and fails closed (#91); M39 browser self-service linking — `/atmospheric link` starts a one-time browser OAuth flow authenticated with a server-only link token, so a player who is online but not linked can bind their own UUID without an operator code; M40 gate-refusal browser link — an unlinked player refused by the armed gate receives a one-time browser URL in the disconnect reason, using the same M39 endpoint and token. History is atomic-scoped and linear (see the hard rules above).

<!-- allowed-agents -->
## Allowed agents

Only **Claude** (Anthropic) and **Mistral** (Mistral AI) may be used as coding agents in this repository. This rule supersedes any earlier agent-attribution guidance in this file.

- Agent-assisted commits must credit only one of those agents:
  - `Co-Authored-By: Claude <noreply@anthropic.com>`
  - `Co-Authored-By: Mistral Vibe <vibe@mistral.ai>`
- Allowed agent footers are `🤖 Generated with [Claude Code](https://claude.com/claude-code)` and `🤖 Generated with [Mistral Vibe](https://mistral.ai)`.
- Do not add `Co-Authored-By:` trailers, `Generated with` footers, author names, or committer identities for any other agent or AI tool.
- Do not credit another agent in commit metadata.
