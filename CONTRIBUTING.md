# Contributing to atmospheric-horizon

AT Protocol (Bluesky) integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post text and screenshots, reply and react, read notifications, and manage atpcraft-compatible server chat without leaving the game. Thanks for wanting to help!

> Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT.

## Getting started

Build per the [README](README.md) (`cargo build`, plus the `atmospheric-worker` sidecar the current release uses). Check the [Roadmap](#roadmap) section before starting: some things intentionally wait on upstream.

## Notes for this project

- **Architecture rule: the injected runtime is std-only, and every network or disk operation happens in the `atmospheric-worker` sidecar.** Keep that boundary — no I/O sneaking into the injected mod half.
- The atpcraft compatibility surface is **data-isolated by design**: upstream lexicons are reimplemented as data with no code copied, precisely because upstream carries no licence. Keep it that way.
- Launcher-based native-mod distribution waits on third-party native-mod support upstream; don't ship around that in a PR.
- Public reads work signed out; posting and record writes need an account. Test writes against a scratch Bluesky account, never your main.
- Every network proto question belongs to specs and lexicons: link the one you're implementing in the PR.

## Before opening a PR

- `cargo` builds clean and the CI badge stays green; you exercised the feature in-game against a scratch account.
- Conventional commits; focused PRs; describe what you tested in-game and what you couldn't.
- AI-assisted PRs welcome; stand behind every line and run the checks yourself.

## Licence

AGPL-3.0 (see [LICENSE](LICENSE)). Contributions accepted under the same terms.