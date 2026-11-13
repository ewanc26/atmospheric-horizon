<p align="center">
  <a href="https://github.com/ewanc26/atmospheric-horizon/actions/workflows/ci.yml"><img src="https://github.com/ewanc26/atmospheric-horizon/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/ewanc26/atmospheric-horizon/releases/latest"><img src="https://img.shields.io/github/v/release/ewanc26/atmospheric-horizon?label=release" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/ewanc26/atmospheric-horizon?label=licence" alt="AGPL-3.0"></a>
  <a href="https://github.com/sponsors/ewanc26"><img src="https://img.shields.io/github/sponsors/ewanc26?logo=githubsponsors&logoColor=white&label=sponsors" alt="Sponsor"></a>
</p>

# atmospheric-horizon

AT Protocol (Bluesky) integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post text and screenshots, reply and react, and read notifications, profiles and public graph data without leaving the game.

> Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

This is the successor to [jollywhoppers-socialsync](https://github.com/ewanc26/jollywhoppers-socialsync), the Paper plugin that linked Minecraft players to AT Protocol identities, rebuilt as a native Horizon mod. It also ports the feature set of [atpcraft](https://git.atforge.org/atforge/atpcraft/ATPcraft) client-side; its lexicons are reimplemented as data, and no code is copied because the upstream repo carries no licence. See [Roadmap](#roadmap).

## Status

**v0.2.0** (13 November 2026) is the current release. The mod is usable in-game today through the `atmospheric-worker` sidecar: the injected runtime is std-only, and every network or disk operation happens in the worker. Public reads work signed out; posting and record writes need a Bluesky account.

Shipped in v0.2.0: OAuth and app-password login, text and screenshot posts, notifications, atpcraft-compatible chat, signed snapshots and skins, a server profile, read sides for lists, labels, presence, skins, chat history and server profiles, social-graph reads, and reply/like/repost.

On `main` for the next release: social-graph writes — follow/unfollow, block/unblock and mute/unmute (`M23`) — and Bluesky profile reads with atpcraft's pronoun precedence (`M24`).

Not yet: launcher registry distribution, which waits on third-party native-mod support upstream ([#28](https://github.com/ewanc26/atmospheric-horizon/issues/28)); label issuance and join-gate enforcement, which stay server-side; and atpcraft's newer server-side surfaces — Colibri bridging, RPG item discovery, full player-data aspect capture/restore and the server account.

## Features

- **Post from the game.** Publish text with `/atmospheric post`, or the latest F2 screenshot or your current position from a keybind, plus optional death messages.
- **Join the conversation.** Reply to, like or repost any Bluesky post, from its AT-URI or its bsky.app URL.
- **Notifications in the HUD.** A digest line, a poll timer and a cursor that survives restarts.
- **atpcraft-compatible records.** Chat messages, Ed25519-endorsed snapshots and skins with blob uploads, and a server descriptor at rkey `self`.
- **Read the public side.** List membership (with a fail-closed gate decision), labels and labeler queries, player presence, skins, chat history, server profiles, profiles with display names and pronouns, and follows/followers/blocks/mutes.
- **Shape your social graph.** Follow, unfollow, block, unblock, mute or unmute an account from its DID or handle.
- **Sign in properly.** OAuth in the system browser (PAR + PKCE + DPoP, loopback redirect), with app passwords as the fallback.
- **Never block the game.** All network and disk I/O lives on one worker thread; the game thread only exchanges JSON lines with it.
- **No surprises.** Writes go to your own PDS, public reads go to the configured AppView or the subject's PDS, and nothing is sent anywhere else.

## Requirements

| | |
| --- | --- |
| Rust | 1.95+ (pinned by `rust-toolchain.toml`) |
| Game | Minecraft: Java Edition 26.3 through the official launcher |
| Account | A Bluesky account (OAuth, or an app password as the fallback) |
| OS | Linux for native staging; the worker is released for Linux x86_64, macOS arm64 and Windows x86_64 |

## Install

The published mod installs through the Horizon launcher's Mods page once it is in the [horizon-mods registry](https://playhorizon.dev/mods). Native mods are built by the launcher against the pinned revisions in `compatibility.json`.

Until then, install the sidecar by hand:

1. Download the archive for your OS from [Releases](https://github.com/ewanc26/atmospheric-horizon/releases) — `atmospheric-worker-vX.Y.Z-linux-x86_64.tar.gz`, `...-macos-aarch64.tar.gz` or `...-windows-x86_64.zip` — plus `atmospheric-horizon-mixins-vX.Y.Z.tar.gz` and `SHA256SUMS`. Check the hash.
2. Put `atmospheric-worker` next to the game executable, or set `ATMOSPHERIC_WORKER_BIN` to its path.
3. Stage `mixins/atmospheric/` and `compatibility.json` from the mixins bundle as described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

The sidecar keeps its files in `<game exe dir>/atmospheric-data/`; set `ATMOSPHERIC_DATA_DIR` to move that folder. (When Horizon spawns the worker through its native storage API, the folder is `horizon-mods/.horizon-mod-data/atmospheric/` instead.) When the game exits, the pipe closes and the worker exits with it. With no worker found, commands are no-ops and the HUD stays hidden.

## Build

```bash
git clone https://github.com/ewanc26/atmospheric-horizon.git
cd atmospheric-horizon
cargo fetch --locked
cargo test --workspace --locked
cargo build --release -p atmospheric --bin atmospheric-worker
```

Copy `target/release/atmospheric-worker` next to the game executable. [CONTRIBUTING.md](CONTRIBUTING.md) lists the fmt, clippy and test gates; [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) covers staging the mixins bundle.

## Usage

### Keybinds

| Key | Action |
| --- | --- |
| `B` | Post your current position |
| `V` | Post the latest screenshot from `screenshots/` |
| `C` | Publish a player-state snapshot |

### Commands

All commands are under `/atmospheric`:

| Command | What it does |
| --- | --- |
| `login [handle]` | Sign in; with a handle, OAuth in the system browser, without one, an app password |
| `logout` | Drop the session |
| `post <text>` | Publish a text post |
| `notifications` | Poll and show the notification digest |
| `chat <text>` | Publish one `at.atpcraft.player.chat` record |
| `snapshot` | Publish a signed player-state snapshot |
| `skin [path] [default\|slim]` | Publish a skin/cape record and its blobs |
| `server` | Publish or refresh the server profile |
| `server-of <did\|handle>` | Read and verify a server profile |
| `profile <did\|handle>` | Read a Bluesky profile and its pronouns |
| `list <at-uri\|bsky.app URL>` | List the members of a Bluesky list |
| `gate <list> <did\|handle>` | Decide list membership, fail-closed |
| `labels <subject>` | Read labels from the AppView |
| `labeler <did> [subject]` | Query a labeler directly |
| `player <did\|handle>` | Read a player's latest verified snapshot |
| `skin-of <did\|handle>` | Read a verified skin and download its blobs |
| `chat-of <did\|handle>` | Read a player's mirrored chat history |
| `follows\|followers\|blocks\|mutes <did\|handle>` | Read a public social graph |
| `follow\|unfollow\|block\|unblock\|mute\|unmute <did\|handle>` | Write one relationship |
| `reply <at-uri> <text>` | Reply to a post |
| `like <at-uri>` | Like a post |
| `repost <at-uri>` | Repost a post |

`bsky.app` post URLs work wherever an `at-uri` is accepted. The `ATMOSPHERIC_*` probes in [`examples/pds_check.rs`](crates/atmospheric/examples/pds_check.rs) exercise the network paths without starting the game.

## Configuration

`config.json` lives in the worker's data folder. The defaults are:

| Key | Default | Purpose |
| --- | --- | --- |
| `pds_url` | `https://bsky.social` | PDS to read from and write to |
| `appview_url` | `https://public.api.bsky.app` | AppView for public reads |
| `identifier` | empty | Handle, email or DID for app-password login |
| `app_password` | empty | App password — never an account password |
| `poll_interval_secs` | `60` | Seconds between notification polls |
| `persist_session` | `true` | Keep the refresh token in `session.json` across restarts |
| `post_death_messages` | `false` | Post a text post when you die |
| `post_chat_messages` | `false` | Mirror every chat line to your repo |
| `chat_server_id` | `horizon-client` | The `server` field on chat records |
| `skin_path`, `cape_path` | empty | Default PNGs for `/atmospheric skin` |
| `skin_model` | `default` | Skin model: `default` or `slim` |
| `oauth_client_id` | empty | Hosted client-metadata URL when loopback registration is unsupported |
| `server_name`, `server_description`, `server_address`, `server_website`, `server_contact` | empty | Server-profile identity; blank omits the field, and `server_name` falls back to the login handle |
| `server_capabilities` | `player-data-sync`, `skin-sync` | Capability list, sorted and deduped on write |
| `game_version` | `26.3` | `minecraftVersion` in the server profile |
| `trusted_server_keys` | `[]` | `{key_id, public_key}` endorsements accepted on reads |

The app password is the only secret in `config.json`; OAuth sessions never store one.

## Roadmap

M1–M21 are shipped, through v0.1.0 and v0.2.0, and M23 (social-graph writes) and M24 (profile reads) are on `main` for the next release; the release history is in [CHANGELOG.md](CHANGELOG.md). What remains is distribution, the server-side halves a client mod cannot host, and the next read/write surfaces.

### Porting atpcraft

atpcraft is a server-side Fabric mod, while Horizon native mods can hook the server side too (`native_tick`/`native_world_lifecycle` run there). Its lexicons are reimplemented here as data — no code is copied, only the wire formats are matched.

| atpcraft feature | atmospheric-horizon |
| --- | --- |
| Bluesky sign-in | **M8**: localhost OAuth login (`/atmospheric login <handle>` — PAR + PKCE + DPoP with a loopback redirect); app-password login remains the fallback via bare `/atmospheric login` |
| Profile pronouns and display names | **M24**: `/atmospheric profile <did\|handle>` reads the AppView's `app.bsky.actor.getProfile` and the direct `app.bsky.actor.profile` record at rkey `self`. The direct record's pronouns win when that read succeeds (even when the field is absent); a failed record read falls back to the AppView, matching atpcraft's precedence. Both reads are unauthenticated |
| Chat messages as records (`at.atpcraft.player.chat`) | **M5**: the chat hook mirrors each line, and the worker publishes TID-keyed records when `post_chat_messages` is on; `/atmospheric chat <text>` always publishes one. **M16** (read side): `/atmospheric chat-of <did\|handle>` lists the collection on the subject's own PDS, unauthenticated, and reports the count plus the newest messages — chat records are unsigned by design |
| Player state snapshots (`at.atpcraft.player.snapshot`, `defs`) | **M6**: the keybind or `/atmospheric snapshot` publishes a self-endorsed record (the worker generates and persists an Ed25519 key; rkey = key id, monotonic version). **M13** (read side): `/atmospheric player <did\|handle>` pages `com.atproto.repo.listRecords`, takes the highest `version`, and verifies the endorsement against your own key or `trusted_server_keys` |
| Reading other players' snapshots (presence) | **M13**: as above — the client-side half of atpcraft's player-data sync |
| Skins and capes as blobs on the PDS | **M9**: skin/cape PNGs are uploaded as blobs and published as an Ed25519-endorsed `at.atpcraft.player.skin` record (rkey = key id, `putRecord` upsert) via `/atmospheric skin [path] [default\|slim]`. **M14** (read side): `/atmospheric skin-of <did\|handle>` verifies the endorsement against your own key or `trusted_server_keys`, then downloads the skin/cape blobs into the mod's `skins/` folder — only when the record verifies |
| Labeler publishing player activity | **M12** (read side only): labels on a subject via the AppView's `labels` fields (`/atmospheric labels <subject>`), and `com.atproto.label.queryLabels` at a labeler's `#atproto_labeler` DID-document endpoint (`/atmospheric labeler <did> [subject]`). Label *issuance* stays server-side — a client mod cannot host the HTTP endpoint |
| Server profile record (`at.atpcraft.server.profile`) | **M10**: published at rkey `self` via `putRecord` with `/atmospheric server` (identity fields from `config.json`, `signingKey` block = the mod's Ed25519 key, `createdAt` preserved across re-publishes). **M19** (read side): `/atmospheric server-of <did\|handle>` lists the collection and reports the server name, MC version, capabilities, and whether its published signing key is trusted, unknown or mismatched |
| Join gate reading a public Bluesky list | **M11** (read side): `/atmospheric list <at-uri \| bsky.app URL>` fetches member DIDs via paginated `app.bsky.graph.getList`. **M15** (the decision): `/atmospheric gate <list> <did\|handle>` reports whether the DID is on the list, fail-closed (enforcement itself stays server-side) |
| Colibri chat bridge | not planned for the client mod: the bridge joins a channel and relays every server chat line, which needs the server thread and the server account's service-auth token |
| RPG item discovery (`equipment.rpg.*`) | not planned for the client mod: discovery scans the server-side inventory and writes provider attestations from the server account |
| Full player-data aspect sync | **M13** reads and verifies snapshots, including their aspect list, but does not capture or restore inventory/advancements/stats. Capture and restore are server-side; the aspect payloads are opaque NBT/JSON blobs |
| Server account and labeler issuance | server-side: registering the PDS account, publishing the labeler service record, serving label routes and enforcing the join gate all need a long-running HTTP server |
| Web server for login pages / lexicons | not planned for the client mod; OAuth sign-in is the alternative |

## Network use

This mod talks to the network **by design**: the AT Protocol services needed to reach and authenticate with your account's PDS, plus the public AppView and labelers — nothing else. All network I/O happens in the spawned local `atmospheric-worker` process, never in the game; the game and the worker exchange plain JSON commands and events over a local pipe only.

### Identity and login

- Handle resolution uses `com.atproto.identity.resolveHandle` on `https://bsky.social`, and DID documents come from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web). The DID document supplies the PDS's `#atproto_pds` service endpoint and a labeler's `#atproto_labeler` endpoint.
- OAuth (M8) discovers the PDS's `/.well-known/oauth-protected-resource` and the authorization server's `/.well-known/oauth-authorization-server`, then sends a pushed authorization request (`POST <authorization server>/oauth/par`) with PKCE and a DPoP key thumbprint. The authorize page opens in your system browser (the URL is always shown first; if no browser can launch, the worker tells you to open it manually), and the redirect is received on `http://127.0.0.1:<port>/` (loopback only). The code is exchanged at the authorization server's token endpoint (`POST <authorization server>/oauth/token`), which is also used for refreshes.
- Every request to the authorization server, and every PDS request while OAuth-logged-in, carries `Authorization: DPoP <access token>` plus a `DPoP` proof JWT (ES256, signed by a per-install P-256 key). The client registers as the loopback client `http://localhost?scope=atproto%20transition%3Ageneric`; if your PDS's authorization server does not support that, set `oauth_client_id` to a hosted copy of `oauth-client-metadata.json`. This repo serves one at `https://ewanc26.github.io/atmospheric-horizon/oauth-client-metadata.json` ([#21](https://github.com/ewanc26/atmospheric-horizon/issues/21)).
- App-password login uses `com.atproto.server.createSession`, checks the session with `com.atproto.server.getSession`, and resumes restarts with `com.atproto.server.refreshSession` (or the OAuth token endpoint's `refresh_token` grant). `/atmospheric logout` drops the session.

### Public reads

All of these are unauthenticated, and the query carries only the subject's DID or the requested record. No credentials or player state are sent.

- **Lists and the join gate (M11/M15):** `app.bsky.graph.getList` (paged, limit 100) against `appview_url` for `/atmospheric list` and `/atmospheric gate`. A failed read is a deny-with-reason, never a silent allow.
- **Labels (M12):** `app.bsky.actor.getProfile`, `app.bsky.feed.getPosts` and `app.bsky.graph.getList` (first page, limit 1 — the view carries the labels) against the same AppView for `/atmospheric labels`; and `com.atproto.label.queryLabels` (paged, limit 100) at the labeler's `#atproto_labeler` endpoint for `/atmospheric labeler`.
- **Profiles (M24):** `app.bsky.actor.getProfile` (query: the subject's DID) against `appview_url`, plus `com.atproto.repo.getRecord` (query: the subject's DID, collection `app.bsky.actor.profile`, rkey `self`) against the subject's own PDS, for `/atmospheric profile`. The direct record's `pronouns` is preferred when that read succeeds; a failed direct read falls back to the AppView value. Nothing is written or stored.
- **Presence (M13):** `com.atproto.repo.listRecords` for `at.atpcraft.player.snapshot` (paged, limit 100) against the subject's own PDS for `/atmospheric player`.
- **Skins (M14):** `com.atproto.repo.listRecords` for `at.atpcraft.player.skin`, then — only when the endorsement verifies — `com.atproto.sync.getBlob` for the skin/cape blob CIDs (query: the subject's DID and the CID) to download the PNGs.
- **Chat history (M16):** `com.atproto.repo.listRecords` for `at.atpcraft.player.chat` against the subject's own PDS for `/atmospheric chat-of`. Chat records are unsigned by design, so nothing is verified.
- **Server profiles (M19):** `com.atproto.repo.listRecords` for `at.atpcraft.server.profile` against the subject's own PDS for `/atmospheric server-of`.
- **Social graphs (M20):** `app.bsky.graph.getFollows`, `getFollowers`, `getBlocks` and `getMutes` (paged, limit 100) against `appview_url` for `/atmospheric follows|followers|blocks|mutes`. The member list comes back to you; nothing about you goes out.
- **Social-graph write lookups (M23):** undoing a follow or block uses `com.atproto.repo.listRecords` for `app.bsky.graph.follow`/`block` against your own PDS (public, unauthenticated) to find records whose `subject` is the target DID. The create/delete/mute calls themselves are in the writes section below.
- **Interaction targets (M21):** `com.atproto.repo.getRecord` from the target author's PDS (query: their DID, collection `app.bsky.feed.post`, and record key) to obtain the post's CID and, for replies, its thread root.

### Writes to your PDS

- Notification polling uses `app.bsky.notification.listNotifications` with a `seenAt` timestamp, on a timer and on `/atmospheric notifications`.
- Text and screenshot posts use `com.atproto.repo.uploadBlob` for the image, then `com.atproto.repo.createRecord` with an `app.bsky.embed.images` embed (or a plain text post). The post-location keybind and death messages publish `app.bsky.feed.post` records the same way; death messages only when `post_death_messages` is enabled.
- Chat lines use `com.atproto.repo.createRecord` in the `at.atpcraft.player.chat` collection, TID-keyed — only when `post_chat_messages` is enabled, or one at a time via `/atmospheric chat <text>`.
- Snapshots use `com.atproto.repo.createRecord` in `at.atpcraft.player.snapshot`, carrying your position/dimension and an Ed25519 endorsement signature.
- Skins use `com.atproto.repo.uploadBlob` for each PNG, `com.atproto.repo.getRecord` to keep other servers' endorsements, then `com.atproto.repo.putRecord` in `at.atpcraft.player.skin` at the endorsing key id.
- The server profile uses `com.atproto.repo.getRecord` to preserve `createdAt`, then `com.atproto.repo.putRecord` in `at.atpcraft.server.profile` at rkey `self`.
- Interactions use `com.atproto.repo.createRecord`: `app.bsky.feed.like` or `app.bsky.feed.repost` with a `subject` strong ref `{uri, cid}`, or an `app.bsky.feed.post` with `text`, `createdAt` and `reply: {root, parent}` strong refs.
- Social-graph writes (M23) resolve the subject to a DID, then:
  - `/atmospheric follow` and `/atmospheric block` write `app.bsky.graph.follow` / `app.bsky.graph.block` with `subject` and `createdAt`, keyed by a fresh TID.
  - `/atmospheric unfollow` and `/atmospheric unblock` find matching records in your own repo (the `listRecords` read above) and delete each with `com.atproto.repo.deleteRecord`.
  - `/atmospheric mute` and `/atmospheric unmute` call `app.bsky.graph.muteActor` / `unmuteActor` on the AppView. Mutes are private and create no record. No new data is stored locally.

### Stored on disk

Everything lives in the worker's own data folder — never in a world save. In sidecar mode that is `<game exe dir>/atmospheric-data/`; under Horizon's native storage API it is `horizon-mods/.horizon-mod-data/atmospheric/`.

- `config.json` holds the settings above, including the PDS URL, identifier, app password, poll interval, post flags, chat server id, skin/cape paths and model, server-profile fields, game version, AppView URL and trusted endorsement keys.
- `signing-key.json` holds the Ed25519 seed for snapshot and skin endorsements, generated once.
- `session.json` holds the refresh token used to resume across restarts. For OAuth sessions it also holds the DPoP private key (JWK) and token endpoint; app passwords are never stored in it. Delete it or use `/atmospheric logout` to revoke a session.
- `cursor.json` (the notification seen-cursor, so restarts do not re-show old notifications) and `snapshot-state.json` (the snapshot version counter, so records never roll back) are not secret.
- `skins/` holds skin/cape PNGs downloaded by `/atmospheric skin-of`, content-addressed by blob CID. They are written only for records whose endorsement verified.

The files that hold secrets are written with owner-only permissions (0600 on unix; the OS file ACLs apply on Windows and macOS). The image you post is read from the game's `screenshots/` folder only when you press the post-screenshot keybind; otherwise the latest path stays in memory. Skin/cape PNGs are read from the path you pass (or `skin_path`/`cape_path`) only at that moment; nothing else is read. The login session is held in memory for the running game; the refresh token is persisted so restarts resume automatically unless `persist_session` is off.

### Nothing else

No telemetry, no analytics, no other endpoints.

## Layout

- `mixins/atmospheric/` — horizon-mixins declarations: the injected std-only `runtime.rs` (which also spawns and bridges the sidecar), its placeholder hook, and `horizon-mixin.toml`.
- `crates/atmospheric/` — the protocol library: `config`, `session`, `oauth` (M8), `actions`, `notifications`, `lists` (M11/M15), `labels` (M12), `presence` (M13), `profiles` (M24), `skins` (M14), `chats` (M16), `servers` (M19), `graph` (M20 reads/M23 writes), `interactions` (M21), `records` (atpcraft lexicon types), `tid`, `signing` (Ed25519 endorsements) and `worker`, plus the `atmospheric-worker` bin and its end-to-end test (`tests/sidecar.rs`).
- `oauth-client-metadata.json` — the OAuth client metadata document to host when an authorization server does not support the loopback client ([#21](https://github.com/ewanc26/atmospheric-horizon/issues/21)).
- `compatibility.json` — the pinned Horizon and horizon-mixins revisions the bundle is built against.
- `CHANGELOG.md` — release notes and the release process.
- `docs/ARCHITECTURE.md` — how the layers connect, for reviewers and contributors.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow and the checks required before opening a pull request. [AGENTS.md](AGENTS.md) carries the rules for AI coding agents.

## Licence

AGPL-3.0-only. See [LICENSE](LICENSE).

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
