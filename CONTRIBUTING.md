# Contributing

Thanks for considering a contribution.

## Project context

- Primary language: Rust (edition 2024)
- Default branch: main
- Issue and pull-request templates live in `.github/`

## Before submitting changes

- Read `README.md` and `docs/ARCHITECTURE.md` first; `AGENTS.md` has the working rules (they apply to humans too).
- Use feature branches and pull requests against `main`. Keep the branch linear: rebase onto `main` instead of merging it in, and expect PRs to be merged with `--rebase` (atomic, scoped commits only — see AGENTS.md's hard rules).
- Keep the layer boundary: the injected `runtime.rs` stays std-only; everything else goes in `crates/atmospheric` with tests.
- Rust modules follow `<scope>/[<subscope>/]<atom>.rs`: directories are scopes, files are single-word atoms, and `mod.rs` only declares a scope. `crates/atmospheric`, `crates/atmospheric-server` and the mixin helper scopes are migrated; keep new code in scopes rather than adding flat files or multi-word atoms.
- Any change that adds an endpoint, a field sent over the wire, or a new kind of stored data must update the README's "Network use" section in the same pull request.
- Run before opening a PR:

```bash
bash scripts/check-layout.sh
bash scripts/test-layout.sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

## Licence

By contributing, you agree your contributions are licensed under AGPL-3.0-only, the repository's licence.
