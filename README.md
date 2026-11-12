# atmospheric-horizon

AT Protocol (Bluesky) integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post screenshots and milestones to Bluesky, and read your notifications, without leaving the game.

Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

This project is the successor to [jollywhoppers-socialsync](https://github.com/ewanc26/jollywhoppers-socialsync) — the Paper plugin that linked Minecraft players to AT Protocol identities — rebuilt as a native Horizon mod. It also aims to port the feature set of [atpcraft](https://atforge.org/products/atpcraft/) (atforge's Minecraft server mod: per-player AT Protocol identity, chat and player-state records, skins as blobs, a labeler, a server profile) to Horizon; see the roadmap below.

## Status

Early scaffold. The protocol library builds and tests standalone; the native injection pipeline follows the [horizon-native-mod-template](https://github.com/horizon-rust/horizon-native-mod-template). Milestones: M1 library skeleton (this commit), M2 mixin wiring + login, M3 screenshots, M4 notifications, M5 registry submission.

### Roadmap: porting atpcraft to Horizon

atpcraft is a server-side Fabric mod; Horizon native mods can hook the server side too (`native_tick`/`native_world_lifecycle` run there), so the port maps onto milestones and later work:

| atpcraft feature | atmospheric-horizon |
|---|---|
| Bluesky sign-in | app-password login (`com.atproto.server.createSession`, M2); a browser OAuth flow would need a tiny local web server on the worker — later milestone |
| Chat messages as records (`at.atpcraft.player.chat`) | later, once the worker is linked in-game (issue #2) |
| Player state snapshots (`at.atpcraft.player.snapshot`, `defs`) | later; position/facing already readable via `native_player` |
| Skins and capes as blobs on the PDS | blob upload proven in M3; skin upload later |
| Labeler publishing player activity | later (`com.atproto.label.*`) |
| Server profile record (`at.atpcraft.server.profile`) | later, server-side |
| Join gate reading a public Bluesky list | later, server-side |
| Web server for login pages / lexicons | not planned for the client mod; OAuth sign-in is the alternative |

## Features

- Post text and in-game screenshots to Bluesky from a keybind
- App-password login (`com.atproto.server.createSession`) via the [ATrium](https://github.com/atrium-rs/atrium) SDK
- Notification digest panel in the HUD
- All network and disk I/O on one worker thread — the game thread never blocks
- Reads and writes the PDS you configure, and nothing else

## Requirements

- Rust 1.95+ (pinned by `rust-toolchain.toml`)
- Linux for native staging (Windows/macOS not validated by the template)
- A Minecraft: Java Edition 26.3 install via the official launcher
- A Bluesky app password

## Install

```bash
git clone https://github.com/ewanc26/atmospheric-horizon.git
cd atmospheric-horizon
cargo fetch --locked
cargo test --workspace --locked
```

The published mod installs through the Horizon launcher's Mods page once it is in the [horizon-mods registry](https://playhorizon.dev/mods); native mods are built by the launcher against the pinned revisions in `compatibility.json`.

## Network use

This mod talks to the network **by design**, and to exactly one place: the PDS you configure.

- Sent to your PDS: your identifier, your app password, the posts you choose to make (text, screenshots), and notification polling requests.
- Stored on disk: your settings and session state, under the mod's own storage folder — never in world saves.
- Nothing else: no telemetry, no analytics, no other endpoints.

## Files

- `mixins/atmospheric/` — horizon-mixins declarations: the injected `runtime.rs` module (std-only), its placeholder hook, and `horizon-mixin.toml`
- `crates/atmospheric/` — the protocol library: `config`, `session`, `actions`, `notifications`, `worker` (all game-free and testable)
- `compatibility.json` — the pinned Horizon and horizon-mixins revisions the bundle is built against
- `docs/ARCHITECTURE.md` — how the layers connect, for reviewers and contributors

## Licence

AGPL-3.0

## Support
If you find this project useful, consider supporting its development:
[![Ko-fi](https://img.shields.io/badge/Ko--fi-F16061?style=for-the-badge&logo=ko-fi&logoColor=white)](https://ko-fi.com/ewancroft)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-30363D?style=for-the-badge&logo=github&logoColor=white)](https://github.com/sponsors/ewanc26)
