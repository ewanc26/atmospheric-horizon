# atmospheric-horizon

AT Protocol (Bluesky) integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post screenshots and milestones to Bluesky, and read your notifications, without leaving the game.

Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

## Status

Early scaffold. The protocol library builds and tests standalone; the native injection pipeline follows the [horizon-native-mod-template](https://github.com/horizon-rust/horizon-native-mod-template). Milestones: M1 library skeleton, M2 worker wiring + login/text posts and the game-side hooks (keybind, `/atmospheric` commands, HUD status line), M3 screenshots (F2, then a keybind posts the latest one), M4 notifications (poll timer + persisted cursor + `/atmospheric notifications`), M5 registry submission.

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

- Sent to your PDS: your identifier and app password (`com.atproto.server.createSession`), a session check after login (`com.atproto.server.getSession`), notification polling (`app.bsky.notification.listNotifications` with a `seenAt` timestamp, on a timer and on `/atmospheric notifications`), and the posts you choose to make: text and screenshots (`com.atproto.repo.uploadBlob` for the image, then `com.atproto.repo.createRecord` with an `app.bsky.embed.images` embed).
- Stored on disk: two files in the mod's own storage folder `horizon-mods/.horizon-mod-data/atmospheric/` — never in world saves. `config.json` (PDS URL, identifier, app password, poll interval, post-death-messages flag) and `cursor.json` (the notification seen-cursor, so restarts do not re-show old notifications). The image you post is read from the game's `screenshots/` folder only when you press the post-screenshot keybind; the path of the latest screenshot is otherwise kept in memory only. The login session is held in memory for the running game and re-created by logging in; persisting it across restarts is planned.
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
