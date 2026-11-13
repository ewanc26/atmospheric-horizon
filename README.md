# atmospheric-horizon

AT Protocol (Bluesky) integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post screenshots and milestones to Bluesky, and read your notifications, without leaving the game.

Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

This project is the successor to [jollywhoppers-socialsync](https://github.com/ewanc26/jollywhoppers-socialsync) — the Paper plugin that linked Minecraft players to AT Protocol identities — rebuilt as a native Horizon mod. It also aims to port the feature set of [atpcraft](https://git.atforge.org/atforge/atpcraft/ATPcraft) (atforge's Minecraft server mod: per-player AT Protocol identity, chat and player-state records, skins as blobs, a labeler, a server profile) to Horizon; see the roadmap below.

## Status

Early scaffold. The protocol library builds and tests standalone; the native injection pipeline follows the [horizon-native-mod-template](https://github.com/horizon-rust/horizon-native-mod-template). Milestones: M1 library skeleton, M2 worker wiring + login/text posts and the game-side hooks (keybind, `/atmospheric` commands, HUD status line), M3 screenshots (F2, then a keybind posts the latest one), M4 notifications (poll timer + persisted cursor + `/atmospheric notifications`), M5 atpcraft port start — chat messages as `at.atpcraft.player.chat` records (TID-keyed, off by default; `/atmospheric chat <text>` publishes one), M6 player-state snapshots as `at.atpcraft.player.snapshot` records (Ed25519-endorsed, keybind or `/atmospheric snapshot`), M7 registry submission, M8 localhost OAuth login (PAR + PKCE + DPoP, `/atmospheric login <handle>`) replacing app passwords, M9 player skin records (`at.atpcraft.player.skin`, blob upload + Ed25519 endorsement, `/atmospheric skin [path] [default|slim]`), M10 server profile record (`at.atpcraft.server.profile` at rkey `self`, `/atmospheric server`), M11 join-gate list reads (`app.bsky.graph.getList` pagination, `/atmospheric list <reference>`), M12 labeler read side (labels on subjects via AppView views, plus `com.atproto.label.queryLabels` at a labeler's `#atproto_labeler` endpoint — `/atmospheric labels <subject>` and `/atmospheric labeler <did> [subject]`), M13 presence reads (any account's latest `at.atpcraft.player.snapshot` record, Ed25519-endorsement-verified — `/atmospheric player <did|handle>`), M14 skin reads (any account's latest `at.atpcraft.player.skin` record, verified, with the blobs downloaded via `com.atproto.sync.getBlob` — `/atmospheric skin-of <did|handle>`), M15 join-gate decision (is a DID on a list, fail-closed — `/atmospheric gate <list> <did|handle>`), M16 chat-history reads (any account's `at.atpcraft.player.chat` records — `/atmospheric chat-of <did|handle>`), M17 sidecar worker — the `atmospheric-worker` binary spawned next to the game consumes the command channel over stdio (JSON-lines), so the mod works in-game today without waiting for horizon-mixins to allow-list the worker crate (#2 option 3), M18 the first tagged release (`v0.1.0`): a changelog, per-OS worker archives, the mixins bundle and checksums built by `.github/workflows/release.yml`, and M19 server-profile reads: any account's `at.atpcraft.server.profile` record read from their PDS and verified against `trusted_server_keys` / own key — `/atmospheric server-of <did|handle>` (#44), and M20 social-graph reads: any account's follows, followers, blocks and mutes via the public AppView — `/atmospheric follows|followers|blocks|mutes <did|handle>` (#48), and M21 interactions: reply to, like or repost any Bluesky post from in-game — `/atmospheric reply <at-uri> <text>`, `/atmospheric like <at-uri>`, `/atmospheric repost <at-uri>` (#53).

### Roadmap: porting atpcraft to Horizon

atpcraft is a server-side Fabric mod; Horizon native mods can hook the server side too (`native_tick`/`native_world_lifecycle` run there), so the port maps onto milestones and later work. atpcraft's lexicon schemas are reimplemented here as data (its repo carries no licence, so no code is copied — only the wire formats are matched):

| atpcraft feature | atmospheric-horizon |
|---|---|
| Bluesky sign-in | **M8**: localhost OAuth login (`/atmospheric login <handle>` — PAR + PKCE + DPoP with a loopback redirect; app-password login remains as the fallback via bare `/atmospheric login`) |
| Chat messages as records (`at.atpcraft.player.chat`) | **M5**: chat hook mirrors each line; the worker publishes TID-keyed records when `post_chat_messages` is on, and `/atmospheric chat <text>` always publishes one. **M16** (read side): `/atmospheric chat-of <did|handle>` lists the collection on the subject's own PDS (unauthenticated) and reports the count plus the newest messages — chat records are unsigned by design, so there is no endorsement to verify |
| Player state snapshots (`at.atpcraft.player.snapshot`, `defs`) | **M6**: keybind or `/atmospheric snapshot` publishes a self-endorsed record (Ed25519 key generated + persisted by the worker; rkey = key id, monotonic version). **M13** (read side): `/atmospheric player <did|handle>` pages `com.atproto.repo.listRecords` for the collection on the subject's own PDS, takes the highest `version`, and verifies the endorsement against your own key or `trusted_server_keys` in `config.json` |
| Reading other players' snapshots (presence) | **M13**: as above — the client-side half of atpcraft's player-data sync |
| Skins and capes as blobs on the PDS | **M9**: skin/cape PNGs uploaded as blobs, published as an Ed25519-endorsed `at.atpcraft.player.skin` record (rkey = key id, `putRecord` upsert) via `/atmospheric skin [path] [default|slim]`. **M14** (read side): `/atmospheric skin-of <did|handle>` lists the collection on the subject's own PDS, verifies the endorsement against your own key or `trusted_server_keys`, and downloads the skin/cape blobs (`com.atproto.sync.getBlob`) into the mod's `skins/` folder — only when the record verifies |
| Labeler publishing player activity | **M12** (read side only): labels on a subject via the AppView view `labels` fields (`/atmospheric labels <subject>`), and `com.atproto.label.queryLabels` at a labeler's `#atproto_labeler` DID-doc endpoint (`/atmospheric labeler <did> [subject]`). Label *issuance* stays server-side — a client mod cannot host the HTTP endpoint |
| Server profile record (`at.atpcraft.server.profile`) | **M10**: published at rkey `self` via `putRecord` with `/atmospheric server` (identity fields from `config.json`, `signingKey` block = the mod's Ed25519 key, `createdAt` preserved across re-publishes). **M19** (read side): `/atmospheric server-of <did|handle>` lists the collection on the subject's own PDS (unauthenticated) and reports the server name, MC version, capabilities, and whether its published signing key is trusted, unknown, or mismatched |
| Join gate reading a public Bluesky list | **M11** (read side): `/atmospheric list <at-uri | bsky.app URL>` fetches member DIDs via paginated `app.bsky.graph.getList` on the public AppView; **M15** (the decision): `/atmospheric gate <list> <did|handle>` reports whether the DID is on the list, fail-closed (enforcement itself stays server-side) |
| Web server for login pages / lexicons | not planned for the client mod; OAuth sign-in is the alternative |

## Features

- Post text and in-game screenshots to Bluesky from a keybind
- Publish chat messages as `at.atpcraft.player.chat` records (atpcraft-compatible, TID-keyed)
- Publish your skin/cape as an `at.atpcraft.player.skin` record (atpcraft-compatible, blob upload + Ed25519 endorsement, M9)
- Publish/refresh an `at.atpcraft.server.profile` descriptor at rkey `self` (atpcraft-compatible, M10)
- Read any account's server descriptor: `/atmospheric server-of <did|handle>` lists `at.atpcraft.server.profile` on their own PDS, checks its published signing key against `trusted_server_keys` and your own key, and displays the server name, Minecraft version, and capabilities (M19)
- Read any account's public social graph: `/atmospheric follows|followers|blocks|mutes <did|handle>` pages `app.bsky.graph.getFollows`/`getFollowers`/`getBlocks`/`getMutes` on the public AppView and reports the count plus the first handles (M20)
- Take part in the conversation: `/atmospheric reply <at-uri> <text>`, `/atmospheric like <at-uri>` and `/atmospheric repost <at-uri>` read the target post's strong ref (and thread root) from its author's PDS and publish the matching record to your own repo (M21)
- Read public Bluesky lists (the join-gate read side): `/atmospheric list <reference>` paginates `app.bsky.graph.getList` and reports the member DIDs (M11), and `/atmospheric gate <list> <did|handle>` decides membership fail-closed (M15)
- Read labels on accounts, posts and lists via the AppView (`/atmospheric labels <subject>`), and query a labeler directly through its DID-document `#atproto_labeler` endpoint (`/atmospheric labeler <did> [subject]`) (M12)
- Read any player's latest server-endorsed snapshot record (presence): `/atmospheric player <did|handle>` lists `at.atpcraft.player.snapshot` on their own PDS and verifies the Ed25519 endorsement against your own key or the configured `trusted_server_keys` (M13)
- Read any player's latest server-endorsed skin record and download the skin/cape PNGs: `/atmospheric skin-of <did|handle>` verifies the record the same way and saves the blobs under the mod's `skins/` folder (M14)
- Read any player's mirrored chat history: `/atmospheric chat-of <did|handle>` lists `at.atpcraft.player.chat` on their own PDS and reports the count plus the newest messages (M16; chat records are unsigned by design)
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

**Sidecar worker (M17).** The game-side hooks talk to a separate worker process, not to code linked into the game (horizon-mixins allow-lists bundle-crate deps to Horizon's `Cargo.lock`, which has no `atrium-api`/`reqwest` — issue #2). Since `v0.1.0` the [releases page](https://github.com/ewanc26/atmospheric-horizon/releases) carries a prebuilt `atmospheric-worker` for Linux x86_64, macOS arm64 and Windows x86_64, plus the mixins bundle and a `SHA256SUMS` file; download the archive for your OS, check its hash, and unpack the binary next to the game executable. To build it yourself:

```bash
cargo build --release -p atmospheric --bin atmospheric-worker
cp target/release/atmospheric-worker <dir of the horizon-client executable>/   # or set ATMOSPHERIC_WORKER_BIN=/path/to/atmospheric-worker
```

The mixins bundle (`atmospheric-horizon-mixins-vX.Y.Z.tar.gz`) holds `mixins/atmospheric/` plus `compatibility.json`, so it can be staged with the pinned Horizon export exactly as described in `docs/ARCHITECTURE.md`.

The worker stores its files (`config.json`, `session.json`, …) in `<game exe dir>/atmospheric-data/` by default — override with `ATMOSPHERIC_DATA_DIR`. When the game exits, the pipe closes and the worker exits with it. With no binary found, the mod's commands are no-ops and the HUD stays hidden.

## Network use

This mod talks to the network **by design**: the AT Protocol services needed to reach and authenticate with your account's PDS — nothing else. All network I/O happens in the spawned local `atmospheric-worker` process (M17), never in the game itself; the game and the worker exchange plain JSON commands/events over a local pipe only.

- Identity resolution for OAuth login (M8) and for bsky.app list URLs (M11), using the public AT Protocol directory services: `com.atproto.identity.resolveHandle` on `https://bsky.social`, the DID document from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web), and the PDS's own `/.well-known/oauth-protected-resource` + authorization server `/.well-known/oauth-authorization-server`.
- Public list reads (M11/M15, the join-gate read side and decision): unauthenticated `app.bsky.graph.getList` (paged, limit 100) against the public AppView `https://public.api.bsky.app` — configurable via `appview_url` in `config.json`; `/atmospheric gate` uses the same read and compares the subject's resolved DID against the members (a read failure is reported as a deny-with-reason, never a silent allow).
- Public label reads (M12, the labeler read side): unauthenticated `app.bsky.actor.getProfile`, `app.bsky.feed.getPosts` and `app.bsky.graph.getList` (first page, limit 1 — the list view itself carries the labels) against the same AppView (`appview_url`), reading the `labels` field of the returned views for `/atmospheric labels <subject>`; and unauthenticated `com.atproto.label.queryLabels` (paged, limit 100) against a labeler's service endpoint for `/atmospheric labeler <did> [subject]` — the endpoint is the `#atproto_labeler` (type `AtprotoLabeler`) service of the labeler DID's document, fetched from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web) like the other DID documents.
- Player snapshot reads (M13, the presence read side): for `/atmospheric player <did|handle>`, handle resolution and the DID-document fetch work exactly as above, then unauthenticated `com.atproto.repo.listRecords` for `at.atpcraft.player.snapshot` (paged, limit 100) against the subject's own PDS — the `#atproto_pds` service endpoint of their DID document. The only value sent in the query is the subject's DID/handle; the records come back to you, nothing about you goes out.
- Player skin reads (M14, the skin-sync read side): for `/atmospheric skin-of <did|handle>`, the same resolution, then unauthenticated `com.atproto.repo.listRecords` for `at.atpcraft.player.skin` (paged, limit 100) against the subject's own PDS, and — only when the record's endorsement verifies — unauthenticated `com.atproto.sync.getBlob` for the skin/cape blob CIDs (query: the subject's DID and the CID) to download the PNGs.
- Chat-history reads (M16, the read side of the M5 chat mirror): for `/atmospheric chat-of <did|handle>`, the same resolution, then unauthenticated `com.atproto.repo.listRecords` for `at.atpcraft.player.chat` (paged, limit 100) against the subject's own PDS. Chat records are unsigned (plain records in the author's repo), so nothing is verified — the query carries only the subject's DID.
- Server profile reads (M19, the read side of the M10 server descriptor): for `/atmospheric server-of <did|handle>`, the same account resolution (handle to DID via directory, then DID doc for the PDS endpoint), then unauthenticated `com.atproto.repo.listRecords` for `at.atpcraft.server.profile` against the subject's own PDS. No credentials or player state are sent.
- Social-graph reads (M20): for `/atmospheric follows|followers|blocks|mutes <did|handle>`, handle resolution as above, then unauthenticated `app.bsky.graph.getFollows`, `getFollowers`, `getBlocks` or `getMutes` (paged, limit 100) against the public AppView at `appview_url`. The query carries only the subject's DID; the member list comes back to you and nothing about you goes out.
- Interactions (M21): for `/atmospheric reply|like|repost <at-uri>` (or a bsky.app post URL), the target post is read unauthenticated with `com.atproto.repo.getRecord` from the author's PDS (query: their DID, collection `app.bsky.feed.post`, and the record key) to obtain its CID and, for replies, its thread root. The interaction is then written to your own PDS with `com.atproto.repo.createRecord`: `app.bsky.feed.like` or `app.bsky.feed.repost` carrying a `subject` strong ref `{uri, cid}`, or an `app.bsky.feed.post` carrying `text`, `createdAt` and `reply: {root, parent}` strong refs. The read sends nothing about you.
- OAuth login (M8): a pushed authorization request (`POST <authorization server>/oauth/par`) with PKCE and a DPoP key thumbprint, the authorize page opened in your system browser (the authorize URL is always shown first, and if no browser can be launched the worker tells you to open the link manually), the redirect received on `http://127.0.0.1:<port>/` (loopback only), and the code exchanged at the authorization server's token endpoint (`POST <authorization server>/oauth/token`, also used for refreshes). Every request to the authorization server and every request to the PDS while OAuth-logged-in carries `Authorization: DPoP <access token>` plus a `DPoP` proof JWT (ES256, signed by a per-install P-256 key). The client registers as the loopback client `http://localhost?scope=atproto%20transition%3Ageneric` (the spec's localhost exception); if your PDS's authorization server does not support it, set `oauth_client_id` in `config.json` to a hosted copy of `oauth-client-metadata.json` — this repo's own GitHub Pages copy is served at `https://ewanc26.github.io/atmospheric-horizon/oauth-client-metadata.json` (#21).
- Sent to your PDS: your identifier and app password (`com.atproto.server.createSession`), a session check after login (`com.atproto.server.getSession`), notification polling (`app.bsky.notification.listNotifications` with a `seenAt` timestamp, on a timer and on `/atmospheric notifications`), session resume across restarts (`com.atproto.server.refreshSession` with the stored refresh token — app-password sessions — or the OAuth token endpoint's `refresh_token` grant with the stored DPoP-bound refresh token, once per game start), and the records you choose to make: text and screenshot posts (`com.atproto.repo.uploadBlob` for the image, then `com.atproto.repo.createRecord` with an `app.bsky.embed.images` embed), chat lines (`com.atproto.repo.createRecord` in the `at.atpcraft.player.chat` collection, TID-keyed — only when `post_chat_messages` is enabled in `config.json`, or one at a time via `/atmospheric chat <text>`), player-state snapshots (`com.atproto.repo.createRecord` in the `at.atpcraft.player.snapshot` collection, keybind or `/atmospheric snapshot`, carrying your position/dimension and an Ed25519 endorsement signature), skin/cape (`com.atproto.repo.uploadBlob` for each PNG, a `com.atproto.repo.getRecord` to keep other servers' endorsements, then `com.atproto.repo.putRecord` in the `at.atpcraft.player.skin` collection at the endorsing key id — `/atmospheric skin [path] [default|slim]`, M9), the server profile (a `com.atproto.repo.getRecord` to preserve `createdAt`, then `com.atproto.repo.putRecord` in the `at.atpcraft.server.profile` collection at rkey `self` — `/atmospheric server`, M10), and death messages (an `app.bsky.feed.post` when you die, only when `post_death_messages` is enabled). `/atmospheric logout` drops the session.
- Stored on disk: files in the mod's own storage folder `horizon-mods/.horizon-mod-data/atmospheric/` — never in world saves. The files that hold secrets — `config.json` (PDS URL, identifier, app password, poll interval, post-death-messages flag, post-chat-messages flag, chat server id, skin/cape paths, skin model, server-profile identity fields/capabilities, game version, AppView URL, trusted server endorsement keys), `signing-key.json` (the Ed25519 seed for snapshot endorsements, generated once) and `session.json` (the refresh token for resuming the session across restarts; for OAuth sessions it also holds the DPoP private key (JWK) and the token endpoint; app passwords are never stored in it) — are written with owner-only permissions (0600 on unix; on Windows/macOS the OS file ACLs apply as usual). Delete `session.json` or use `/atmospheric logout` to revoke a session. `cursor.json` (the notification seen-cursor, so restarts do not re-show old notifications) and `snapshot-state.json` (the snapshot version counter, so records never roll back) are not secret. Verified skin/cape PNGs downloaded by `/atmospheric skin-of` land in the `skins/` subfolder of the mod's storage folder (content-addressed by blob CID; not secret, and only written for records whose endorsement verified). The image you post is read from the game's `screenshots/` folder only when you press the post-screenshot keybind; the path of the latest screenshot is otherwise kept in memory only. The skin/cape PNGs you publish with `/atmospheric skin` are read from the path you pass (or `skin_path`/`cape_path` in `config.json`) only at that moment; nothing else is read. The login session is held in memory for the running game; the refresh token is persisted (see above) so restarts resume automatically unless `persist_session` is off.
- Nothing else: no telemetry, no analytics, no other endpoints.

## Files

- `mixins/atmospheric/` — horizon-mixins declarations: the injected `runtime.rs` module (std-only; also spawns/bridges the M17 sidecar worker), its placeholder hook, and `horizon-mixin.toml`
- `crates/atmospheric/` — the protocol library: `config`, `session`, `oauth` (M8: DPoP/PKCE/PAR/loopback/token flow), `actions`, `notifications`, `lists` (M11/M15), `labels` (M12), `presence` (M13), `skins` (M14), `chats` (M16), `servers` (M19: server profile reads), `records` (atpcraft lexicon types), `tid` (record keys), `signing` (Ed25519 endorsements), `worker` (all game-free and testable); plus the `atmospheric-worker` sidecar bin (`src/bin/atmospheric-worker.rs`, M17) and its end-to-end test (`tests/sidecar.rs`)
- `oauth-client-metadata.json` — the OAuth client metadata document to host when an authorization server does not support the loopback client; this repo serves it via GitHub Pages at `https://ewanc26.github.io/atmospheric-horizon/oauth-client-metadata.json` (#21) — point `oauth_client_id` at that URL
- `compatibility.json` — the pinned Horizon and horizon-mixins revisions the bundle is built against
- `CHANGELOG.md` — release notes and the release process (tag `vX.Y.Z`; the release workflow builds the worker archives and mixins bundle)
- `docs/ARCHITECTURE.md` — how the layers connect, for reviewers and contributors

## Licence

AGPL-3.0

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow and the checks required before opening a pull request. [AGENTS.md](AGENTS.md) carries the rules for AI coding agents.

## Support

If you find this project useful, consider supporting its development:
[![Ko-fi](https://img.shields.io/badge/Ko--fi-F16061?style=for-the-badge&logo=ko-fi&logoColor=white)](https://ko-fi.com/ewancroft)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-30363D?style=for-the-badge&logo=github&logoColor=white)](https://github.com/sponsors/ewanc26)

## Star History

<a href="https://www.star-history.com/?repos=ewanc26%2Fatmospheric-horizon&type=date&legend=bottom-right">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=ewanc26/atmospheric-horizon&type=date&theme=dark&legend=bottom-right" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=ewanc26/atmospheric-horizon&type=date&legend=bottom-right" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=ewanc26/atmospheric-horizon&type=date&legend=bottom-right" />
 </picture>
</a>
