# atmospheric-horizon

AT Protocol (Bluesky) integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post screenshots and milestones to Bluesky, and read your notifications, without leaving the game.

Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

This project is the successor to [jollywhoppers-socialsync](https://github.com/ewanc26/jollywhoppers-socialsync) — the Paper plugin that linked Minecraft players to AT Protocol identities — rebuilt as a native Horizon mod. It also aims to port the feature set of [atpcraft](https://git.atforge.org/atforge/atpcraft/ATPcraft) (atforge's Minecraft server mod: per-player AT Protocol identity, chat and player-state records, skins as blobs, a labeler, a server profile) to Horizon; see the roadmap below.

## Status

Early scaffold. The protocol library builds and tests standalone; the native injection pipeline follows the [horizon-native-mod-template](https://github.com/horizon-rust/horizon-native-mod-template). Milestones: M1 library skeleton, M2 worker wiring + login/text posts and the game-side hooks (keybind, `/atmospheric` commands, HUD status line), M3 screenshots (F2, then a keybind posts the latest one), M4 notifications (poll timer + persisted cursor + `/atmospheric notifications`), M5 atpcraft port start — chat messages as `at.atpcraft.player.chat` records (TID-keyed, off by default; `/atmospheric chat <text>` publishes one), M6 player-state snapshots as `at.atpcraft.player.snapshot` records (Ed25519-endorsed, keybind or `/atmospheric snapshot`), M7 registry submission, M8 localhost OAuth login (PAR + PKCE + DPoP, `/atmospheric login <handle>`) replacing app passwords, M9 player skin records (`at.atpcraft.player.skin`, blob upload + Ed25519 endorsement, `/atmospheric skin [path] [default|slim]`), M10 server profile record (`at.atpcraft.server.profile` at rkey `self`, `/atmospheric server`), M11 join-gate list reads (`app.bsky.graph.getList` pagination, `/atmospheric list <reference>`), M12 labeler read side (labels on subjects via AppView views, plus `com.atproto.label.queryLabels` at a labeler's `#atproto_labeler` endpoint — `/atmospheric labels <subject>` and `/atmospheric labeler <did> [subject]`).

### Roadmap: porting atpcraft to Horizon

atpcraft is a server-side Fabric mod; Horizon native mods can hook the server side too (`native_tick`/`native_world_lifecycle` run there), so the port maps onto milestones and later work. atpcraft's lexicon schemas are reimplemented here as data (its repo carries no licence, so no code is copied — only the wire formats are matched):

| atpcraft feature | atmospheric-horizon |
|---|---|
| Bluesky sign-in | **M8**: localhost OAuth login (`/atmospheric login <handle>` — PAR + PKCE + DPoP with a loopback redirect; app-password login remains as the fallback via bare `/atmospheric login`) |
| Chat messages as records (`at.atpcraft.player.chat`) | **M5**: chat hook mirrors each line; the worker publishes TID-keyed records when `post_chat_messages` is on, and `/atmospheric chat <text>` always publishes one |
| Player state snapshots (`at.atpcraft.player.snapshot`, `defs`) | **M6**: keybind or `/atmospheric snapshot` publishes a self-endorsed record (Ed25519 key generated + persisted by the worker; rkey = key id, monotonic version) |
| Skins and capes as blobs on the PDS | **M9**: skin/cape PNGs uploaded as blobs, published as an Ed25519-endorsed `at.atpcraft.player.skin` record (rkey = key id, `putRecord` upsert) via `/atmospheric skin [path] [default|slim]` |
| Labeler publishing player activity | **M12** (read side only): labels on a subject via the AppView view `labels` fields (`/atmospheric labels <subject>`), and `com.atproto.label.queryLabels` at a labeler's `#atproto_labeler` DID-doc endpoint (`/atmospheric labeler <did> [subject]`). Label *issuance* stays server-side — a client mod cannot host the HTTP endpoint |
| Server profile record (`at.atpcraft.server.profile`) | **M10**: published at rkey `self` via `putRecord` with `/atmospheric server` (identity fields from `config.json`, `signingKey` block = the mod's Ed25519 key, `createdAt` preserved across re-publishes) |
| Join gate reading a public Bluesky list | **M11** (read side): `/atmospheric list <at-uri | bsky.app URL>` fetches member DIDs via paginated `app.bsky.graph.getList` on the public AppView (enforcement server-side later) |
| Web server for login pages / lexicons | not planned for the client mod; OAuth sign-in is the alternative |

## Features

- Post text and in-game screenshots to Bluesky from a keybind
- Publish chat messages as `at.atpcraft.player.chat` records (atpcraft-compatible, TID-keyed)
- Publish your skin/cape as an `at.atpcraft.player.skin` record (atpcraft-compatible, blob upload + Ed25519 endorsement, M9)
- Publish/refresh an `at.atpcraft.server.profile` descriptor at rkey `self` (atpcraft-compatible, M10)
- Read public Bluesky lists (the join-gate read side): `/atmospheric list <reference>` paginates `app.bsky.graph.getList` and reports the member DIDs (M11)
- Read labels on accounts, posts and lists via the AppView (`/atmospheric labels <subject>`), and query a labeler directly through its DID-document `#atproto_labeler` endpoint (`/atmospheric labeler <did> [subject]`) (M12)
- OAuth login in the system browser (`/atmospheric login <handle>`, M8 — DPoP-bound tokens, loopback redirect), with app-password login (`com.atproto.server.createSession`) as the fallback via the [ATrium](https://github.com/atrium-rs/atrium) SDK
- Notification digest panel in the HUD
- All network and disk I/O on one worker thread — the game thread never blocks
- Reads and writes the PDS you configure, and nothing else

## Requirements

- Rust 1.95+ (pinned by `rust-toolchain.toml`)
- Linux for native staging (Windows/macOS not validated by the template)
- A Minecraft: Java Edition 26.3 install via the official launcher
- A Bluesky account (OAuth in the system browser, or an app password as the fallback)

## Install

```bash
git clone https://github.com/ewanc26/atmospheric-horizon.git
cd atmospheric-horizon
cargo fetch --locked
cargo test --workspace --locked
```

The published mod installs through the Horizon launcher's Mods page once it is in the [horizon-mods registry](https://playhorizon.dev/mods); native mods are built by the launcher against the pinned revisions in `compatibility.json`.

## Network use

This mod talks to the network **by design**: the AT Protocol services needed to reach and authenticate with your account's PDS — nothing else.

- Identity resolution for OAuth login (M8) and for bsky.app list URLs (M11), using the public AT Protocol directory services: `com.atproto.identity.resolveHandle` on `https://bsky.social`, the DID document from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web), and the PDS's own `/.well-known/oauth-protected-resource` + authorization server `/.well-known/oauth-authorization-server`.
- Public list reads (M11, the join-gate read side): unauthenticated `app.bsky.graph.getList` (paged, limit 100) against the public AppView `https://public.api.bsky.app` — configurable via `appview_url` in `config.json`.
- Public label reads (M12, the labeler read side): unauthenticated `app.bsky.actor.getProfile`, `app.bsky.feed.getPosts` and `app.bsky.graph.getList` (first page, limit 1 — the list view itself carries the labels) against the same AppView (`appview_url`), reading the `labels` field of the returned views for `/atmospheric labels <subject>`; and unauthenticated `com.atproto.label.queryLabels` (paged, limit 100) against a labeler's service endpoint for `/atmospheric labeler <did> [subject]` — the endpoint is the `#atproto_labeler` (type `AtprotoLabeler`) service of the labeler DID's document, fetched from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web) like the other DID documents.
- OAuth login (M8): a pushed authorization request (`POST <authorization server>/oauth/par`) with PKCE and a DPoP key thumbprint, the authorize page opened in your system browser (the authorize URL is always shown first, and if no browser can be launched the worker tells you to open the link manually), the redirect received on `http://127.0.0.1:<port>/` (loopback only), and the code exchanged at the authorization server's token endpoint (`POST <authorization server>/oauth/token`, also used for refreshes). Every request to the authorization server and every request to the PDS while OAuth-logged-in carries `Authorization: DPoP <access token>` plus a `DPoP` proof JWT (ES256, signed by a per-install P-256 key). The client registers as the loopback client `http://localhost?scope=atproto%20transition%3Ageneric` (the spec's localhost exception); if your PDS's authorization server does not support it, set `oauth_client_id` in `config.json` to a hosted copy of `oauth-client-metadata.json` (e.g. GitHub Pages).
- Sent to your PDS: your identifier and app password (`com.atproto.server.createSession`), a session check after login (`com.atproto.server.getSession`), notification polling (`app.bsky.notification.listNotifications` with a `seenAt` timestamp, on a timer and on `/atmospheric notifications`), session resume across restarts (`com.atproto.server.refreshSession` with the stored refresh token — app-password sessions — or the OAuth token endpoint's `refresh_token` grant with the stored DPoP-bound refresh token, once per game start), and the records you choose to make: text and screenshot posts (`com.atproto.repo.uploadBlob` for the image, then `com.atproto.repo.createRecord` with an `app.bsky.embed.images` embed), chat lines (`com.atproto.repo.createRecord` in the `at.atpcraft.player.chat` collection, TID-keyed — only when `post_chat_messages` is enabled in `config.json`, or one at a time via `/atmospheric chat <text>`), player-state snapshots (`com.atproto.repo.createRecord` in the `at.atpcraft.player.snapshot` collection, keybind or `/atmospheric snapshot`, carrying your position/dimension and an Ed25519 endorsement signature), skin/cape (`com.atproto.repo.uploadBlob` for each PNG, a `com.atproto.repo.getRecord` to keep other servers' endorsements, then `com.atproto.repo.putRecord` in the `at.atpcraft.player.skin` collection at the endorsing key id — `/atmospheric skin [path] [default|slim]`, M9), the server profile (a `com.atproto.repo.getRecord` to preserve `createdAt`, then `com.atproto.repo.putRecord` in the `at.atpcraft.server.profile` collection at rkey `self` — `/atmospheric server`, M10), and death messages (an `app.bsky.feed.post` when you die, only when `post_death_messages` is enabled). `/atmospheric logout` drops the session.
- Stored on disk: files in the mod's own storage folder `horizon-mods/.horizon-mod-data/atmospheric/` — never in world saves. The files that hold secrets — `config.json` (PDS URL, identifier, app password, poll interval, post-death-messages flag, post-chat-messages flag, chat server id, skin/cape paths, skin model, server-profile identity fields/capabilities, game version, AppView URL), `signing-key.json` (the Ed25519 seed for snapshot endorsements, generated once) and `session.json` (the refresh token for resuming the session across restarts; for OAuth sessions it also holds the DPoP private key (JWK) and the token endpoint; app passwords are never stored in it) — are written with owner-only permissions (0600 on unix; on Windows/macOS the OS file ACLs apply as usual). Delete `session.json` or use `/atmospheric logout` to revoke a session. `cursor.json` (the notification seen-cursor, so restarts do not re-show old notifications) and `snapshot-state.json` (the snapshot version counter, so records never roll back) are not secret. The image you post is read from the game's `screenshots/` folder only when you press the post-screenshot keybind; the path of the latest screenshot is otherwise kept in memory only. The skin/cape PNGs you publish with `/atmospheric skin` are read from the path you pass (or `skin_path`/`cape_path` in `config.json`) only at that moment; nothing else is read. The login session is held in memory for the running game; the refresh token is persisted (see above) so restarts resume automatically unless `persist_session` is off.
- Nothing else: no telemetry, no analytics, no other endpoints.

## Files

- `mixins/atmospheric/` — horizon-mixins declarations: the injected `runtime.rs` module (std-only), its placeholder hook, and `horizon-mixin.toml`
- `crates/atmospheric/` — the protocol library: `config`, `session`, `oauth` (M8: DPoP/PKCE/PAR/loopback/token flow), `actions`, `notifications`, `lists` (M11), `labels` (M12), `records` (atpcraft lexicon types), `tid` (record keys), `signing` (Ed25519 endorsements), `worker` (all game-free and testable)
- `oauth-client-metadata.json` — the OAuth client metadata document to host (e.g. via GitHub Pages) when an authorization server does not support the loopback client; point `oauth_client_id` at its URL
- `compatibility.json` — the pinned Horizon and horizon-mixins revisions the bundle is built against
- `docs/ARCHITECTURE.md` — how the layers connect, for reviewers and contributors

## Licence

AGPL-3.0

## Support
If you find this project useful, consider supporting its development:
[![Ko-fi](https://img.shields.io/badge/Ko--fi-F16061?style=for-the-badge&logo=ko-fi&logoColor=white)](https://ko-fi.com/ewancroft)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-30363D?style=for-the-badge&logo=github&logoColor=white)](https://github.com/sponsors/ewanc26)
