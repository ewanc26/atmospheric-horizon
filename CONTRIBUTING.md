# Contributing

Thanks for considering a contribution.

- Read `README.md` and `docs/ARCHITECTURE.md` first; `AGENTS.md` has the working rules (they apply to humans too).
- Use feature branches and pull requests against `main`.
- Keep the layer boundary: the injected `runtime.rs` stays std-only; everything else goes in `crates/atmospheric` with tests.
- Any change that adds an endpoint, a field sent over the wire, or a new kind of stored data must update the README's "Network use" section in the same pull request.
- Run before opening a PR:

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

## Licence

By contributing, you agree your contributions are licensed under AGPL-3.0-only, the repository's licence.
