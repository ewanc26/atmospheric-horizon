## What

<!-- One or two sentences. Link the issue it closes (e.g. "Closes #48"). -->

## Why

<!-- The problem this solves, or the reason for the change. -->

## Checklist

- [ ] `bash scripts/check-layout.sh` passes
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace --locked` passes
- [ ] `mixins/atmospheric/runtime.rs` is still std-only; all network and disk I/O is in the worker
- [ ] Any new endpoint, wire field or stored data updates `README.md` ("Network use") and `docs/ARCHITECTURE.md`
- [ ] `CHANGELOG.md` and/or `AGENTS.md` updated where a player or contributor would notice
- [ ] No secrets, credentials, binaries, exported Horizon sources or Minecraft assets are committed
- [ ] Commits are atomic and scoped; the branch is rebased (merged with `--rebase`, never squash)

## Notes for reviewers

<!-- Anything specific to look out for, edge cases, or follow-up work. -->
