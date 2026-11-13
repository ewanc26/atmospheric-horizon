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

## Git history and pull requests (hard rules)

- **Atomic, scoped commits.** One logical change per commit, with a subject that says what and why (`type: what — detail`). No WIP, fixup, or "misc" commits on `main`; each commit should leave the tree buildable and tests passing where feasible. Split large work into a sequence of atomic commits on the PR branch.
- **Rebase PRs, never merge commits into them.** Keep a PR branch linear: `git rebase` onto the target branch (resolving conflicts as they come) and push the branch with `--force-with-lease`. Do not `git merge main` into a feature branch.
- **Merge PRs with rebase only.** `gh pr merge <n> --rebase`. Squash merges and merge commits are not allowed on `main` — they destroy the atomic commits. Branch protection on `main` enforces linear history (`required_linear_history`) plus the `check` status check; do not weaken it.
- **History rewrites of `main` are exceptional.** They require explicit owner authorization in the PR/issue first. Because `main` forbids force-pushes, a rewrite needs a temporary, announced change to the branch protection (`allow_force_pushes: true`), a single `--force-with-lease` push of the rewritten branch, and immediate restoration of `allow_force_pushes: false`.

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

M1 scaffold; M2 worker wiring (login + posts, game-side hooks); docs (successor + atpcraft roadmap); M3 screenshot posts; M4 notification polling; M5 atpcraft chat records; M6 snapshots; M8 OAuth; M9 skins; M10 server profile; M11 list reads; M12 label reads (#29 read side); M13 presence reads (#31); M14 skin reads (#33); M15 join-gate decision (#36); M16 chat-history reads (#38); M17 sidecar worker process — the game spawns `atmospheric-worker` next to the executable and bridges the channel over stdio JSON-lines (#40, issue #2 option 3); M18 v0.1.0 release — changelog, tag-driven per-OS worker archives and the mixins bundle (#42); M19 server-profile reads (#44); M20 social-graph reads (#48); M21 reply/like/repost interactions (#53); repository conventions — shared issue/PR templates, gitattributes, README sections (#50); M22 v0.2.0 release (#55); M23 social-graph writes — follow/unfollow, block/unblock and mute/unmute (#60); M24 profile reads — AppView profile plus direct record, with atpcraft's pronoun precedence (#63). History is atomic-scoped and linear (see the hard rules above).

<!-- allowed-agents -->
## Allowed agents

Only **Claude** (Anthropic) and **Mistral** (Mistral AI) may be used as coding agents in this repository. This rule supersedes any earlier agent-attribution guidance in this file.

- Agent-assisted commits must credit only one of those agents:
  - `Co-Authored-By: Claude <noreply@anthropic.com>`
  - `Co-Authored-By: Mistral Vibe <vibe@mistral.ai>`
- Allowed agent footers are `🤖 Generated with [Claude Code](https://claude.com/claude-code)` and `🤖 Generated with [Mistral Vibe](https://mistral.ai)`.
- Do not add `Co-Authored-By:` trailers, `Generated with` footers, author names, or committer identities for any other agent or AI tool.
- Do not credit another agent in commit metadata.
