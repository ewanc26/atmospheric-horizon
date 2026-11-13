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

On `main` for the next release: social-graph writes — follow/unfollow, block/unblock and mute/unmute (`M23`) — Bluesky profile reads with atpcraft's pronoun precedence (`M24`), a separate, persisted server account with its own OAuth session and server-profile publishing (`M25`), the Colibri chat-bridge core (`M26`), its membership/rotation repair (`M27`), server event notices (`M28`), the presence fallback/pending-approval notice (`M29`), the local Colibri profile prompt (`M30`), Colibri reconnect/send throttling (`M31`), local player Colibri membership (`M32`), Colibri incoming chat hygiene (`M33`), Colibri status detail (`M34`) and the optional server-side identity service (`M35`).

Not yet: launcher registry distribution, which waits on third-party native-mod support upstream ([#28](https://github.com/ewanc26/atmospheric-horizon/issues/28)); label issuance, join-gate enforcement, server-wide Colibri profile prompts (M30 covers the local player), RPG item discovery and full player-data aspect capture/restore. M25 supplies the server account, M26/M27 run the Colibri bridge, and M35 supplies the shared UUID-to-DID map; enforcement, issuance and capture remain separate server-side milestones.

## Features

- **Post from the game.** Publish text with `/atmospheric post`, or the latest F2 screenshot or your current position from a keybind, plus optional death messages.
- **Join the conversation.** Reply to, like or repost any Bluesky post, from its AT-URI or its bsky.app URL.
- **Notifications in the HUD.** A digest line, a poll timer and a cursor that survives restarts.
- **atpcraft-compatible records.** Chat messages, Ed25519-endorsed snapshots and skins with blob uploads, and a server descriptor at rkey `self`.
- **Read the public side.** List membership (with a fail-closed gate decision), labels and labeler queries, player presence, skins, chat history, server profiles, profiles with display names and pronouns, and follows/followers/blocks/mutes.
- **Shape your social graph.** Follow, unfollow, block, unblock, mute or unmute an account from its DID or handle.
- **Sign in properly.** OAuth in the system browser (PAR + PKCE + DPoP, loopback redirect), with app passwords as the fallback.
- **Run a server account.** `/atmospheric server-account login <handle>` gives the server its own persisted OAuth identity, separate from the player, and can publish the server descriptor from that account.
- **Link players across the server.** The optional `atmospheric-server` process maps canonical Minecraft UUIDs to ATProto DIDs, with public reads, admin-token writes and short-lived one-time pairing codes. Links survive restarts in a local JSON document; the service makes no outbound requests.
- **Bridge a Colibri channel.** With `colibri_bridge_enabled`, the server account declares its community membership, opens the Colibri event stream and rotates its token before expiry; channel messages appear in game chat, game chat is published into the channel as your player account, and join/leave/death/advancement lines go out as the server account. Hidden or rewritten join/leave lines fall back to the connection event, and a new membership that needs moderator approval gets a HUD notice. The stream can be reconnected without dropping the bridge, and player messages are capped at 20 per rolling minute. The local player's own community membership is repaired when the bridge starts or they log in, and a new gated join gets atpcraft's pending-approval line.
- **Never block the game.** All network and disk I/O lives on one worker thread; the game thread only exchanges JSON lines with it.
- **No surprises.** Writes go to your own PDS, public reads go to the configured AppView or the subject's PDS, and nothing is sent anywhere else.

## Requirements

| | |
| --- | --- |
| Rust | 1.95+ (pinned by `rust-toolchain.toml`) |
| Game | Minecraft: Java Edition 26.3 through the official launcher |
| Account | A Bluesky account (OAuth, or an app password as the fallback) |
| OS | Linux for native staging; the worker is released for Linux x86_64, macOS arm64 and Windows x86_64 |
| Docker | Optional, for the server-side identity service and its end-to-end test |

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
cargo build --release -p atmospheric-server
```

Copy `target/release/atmospheric-worker` next to the game executable. The optional `atmospheric-server` binary is not installed with the game: run it on the Minecraft server host or in Docker as described below. [CONTRIBUTING.md](CONTRIBUTING.md) lists the fmt, clippy and test gates; [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) covers staging the mixins bundle.

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
| `server` | Publish or refresh the server profile from the player account |
| `server-account login <handle>\|status\|logout\|publish` | Manage the separate server account and publish the server profile from it |
| `colibri start\|stop\|reconnect\|status\|send <text>` | Start, stop, reconnect, inspect or write to the Colibri chat bridge |
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
| `colibri_bridge_enabled` | `false` | Arm the bridge after server-account login |
| `colibri_channel_uri` | empty | Channel AT-URI or Colibri app link |
| `colibri_appview_url` | `https://api.colibri.social` | Colibri AppView base URL |
| `colibri_appview_did` | `did:web:api.colibri.social` | AppView service DID; `#colibri_appview` is appended for the audience |
| `colibri_server_events_enabled` | `true` | Relay server join/leave/death/advancement lines into the channel |
| `colibri_server_events_fallback_enabled` | `true` | Fall back to the connection event when a chat mod hides/rewrites a join or leave line |

The app password is the only secret in `config.json`; OAuth sessions never store one.

## Server-side identity service

The optional `atmospheric-server` process supplies the shared Minecraft UUID → ATProto DID map that a client cannot build on its own. It is a separate HTTP service, not part of the game process, and makes no outbound requests.

| Variable | Default | Purpose |
| --- | --- | --- |
| `ATMOSPHERIC_SERVER_ADDR` | `0.0.0.0:8787` | Listen address |
| `ATMOSPHERIC_SERVER_DATA_DIR` | `./atmospheric-server-data` | Folder containing `links.json` |
| `ATMOSPHERIC_SERVER_TOKEN` | unset | Admin bearer token; unset disables all admin routes and pairing creation |

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | public | Status, version, link count and live pairing count |
| `GET /v1/links/{uuid}` | public | Resolve one player's link |
| `GET /v1/links` | admin | List every link |
| `PUT /v1/links/{uuid}` | admin | Create or replace a link directly |
| `DELETE /v1/links/{uuid}` | admin | Remove a link |
| `POST /v1/pairings` | admin | Issue a one-time pairing code for a player |
| `POST /v1/pairings/claim` | pairing code | Bind the code's player to a DID and handle |

Direct operator write:

```bash
curl -X PUT http://127.0.0.1:8787/v1/links/00000000-0000-4000-8000-000000000001 \
  -H "Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"player_name":"Alex","did":"did:plc:...","handle":"alex.example"}'
```

Pairing flow:

```bash
curl -X POST http://127.0.0.1:8787/v1/pairings \
  -H "Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"player_uuid":"00000000-0000-4000-8000-000000000001","player_name":"Alex","ttl_seconds":600}'
# {"code":"ABCD-EFGH","expires_at":1760000000}

curl -X POST http://127.0.0.1:8787/v1/pairings/claim \
  -H 'content-type: application/json' \
  -d '{"code":"ABCD-EFGH","did":"did:plc:...","handle":"alex.example"}'
```

Codes are one-time, default to ten minutes and accept 60–3600 seconds. Reissuing a code for the same UUID invalidates the previous one. Links and pending codes are written through an owner-only temporary file and an atomic rename on Unix (the Docker target) to `links.json`; a failed write leaves the old state in memory and on disk. Run the service in Docker with `docker compose -f docker/compose.yaml up --build` after setting `ATMOSPHERIC_SERVER_TOKEN`, or run the end-to-end test with `bash docker/server-test.sh`.

M35 is the mapping foundation only. Join-gate enforcement, labeler issuance, RPG inventory scanning and aspect capture/restore are not implemented, and the game client does not consume this API yet.

## Roadmap

M1–M21 are shipped, through v0.1.0 and v0.2.0, and M23 (social-graph writes), M24 (profile reads), M25 (server account), M26 (Colibri bridge core), M27 (Colibri membership/rotation), M28 (server event notices), M29 (presence fallback/pending-approval notice), M30 (local Colibri profile prompt), M31 (Colibri reconnect/send throttle), M32 (local Colibri player membership), M33 (Colibri chat hygiene), M34 (Colibri status detail) and M35 (server-side identity service) are on `main` for the next release; the release history is in [CHANGELOG.md](CHANGELOG.md). What remains is distribution, enforcement/issuance/capture on top of the server-side map, and the next read/write surfaces.

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
| Colibri chat bridge | **M26–M34**: `/atmospheric colibri start\|stop\|reconnect\|status\|send <text>` parses a channel AT-URI or app link, declares the server account's community membership, mints and rotates the service-auth token, holds the `social.colibri.sync.subscribeEvents` WebSocket, shows channel messages in game chat, publishes game chat as the player account, and relays server join/leave/death/advancement broadcasts as the server account. **M29** adds the 3-second connection-event fallback for hidden/rewritten join/leave lines and a pending-approval HUD notice when the community requires it. **M30** checks the local player's `social.colibri.actor.profile` when the bridge starts and shows atpcraft's setup prompt if it is absent; **M35** now supplies the server-wide identity map, but the client-side check for other players is not wired yet. **M31** adds `/atmospheric colibri reconnect` and atpcraft's 20-per-minute player send limit. **M32** writes the local player's own membership when the bridge starts or they log in, and shows atpcraft's pending line for a new approval-gated membership. **M33** applies atpcraft's control/whitespace cleanup, 64-character handle cap and 512-character text cap to incoming messages. **M34** carries the community URI and shows it in the bridge status |
| RPG item discovery (`equipment.rpg.*`) | not implemented yet: discovery scans the server-side inventory and writes provider attestations from the server account, which **M25** now supplies |
| Full player-data aspect sync | **M13** reads and verifies snapshots, including their aspect list, but does not capture or restore inventory/advancements/stats. Capture and restore are server-side and remain to be implemented; the aspect payloads are opaque NBT/JSON blobs |
| Server account | **M25**: `/atmospheric server-account login <handle>` runs a separate localhost OAuth flow and keeps the result in `server-session.json`; `status`, `logout` and `publish` manage it. The server profile can then be published as that account (`/atmospheric server-account publish`), as atpcraft's server does. It is the identity foundation for the remaining server-side work |
| Server-wide player identity map | **M35**: optional `atmospheric-server` process with public UUID reads, admin-token writes, one-time pairing claims and atomic persistence. It is the shared mapping the server-wide profile, gate and player-data surfaces need; those consumers are not implemented yet |
| Labeler issuance | **M12** covers reads only; issuance still needs the labeler service record, signed labels, an HTTP endpoint and a PLC identity update, so it is not implemented yet |
| Web server for login pages / lexicons | not implemented yet; OAuth sign-in remains the alternative for players |

## Network use

This mod talks to the network **by design**: the AT Protocol services needed to reach and authenticate with your account's PDS, plus the public AppView and labelers — nothing else. All network I/O happens in the spawned local `atmospheric-worker` process, never in the game; the game and the worker exchange plain JSON commands and events over a local pipe only.

### Identity and login

- Handle resolution uses `com.atproto.identity.resolveHandle` on `https://bsky.social`, and DID documents come from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web). The DID document supplies the PDS's `#atproto_pds` service endpoint and a labeler's `#atproto_labeler` endpoint.
- OAuth (M8) discovers the PDS's `/.well-known/oauth-protected-resource` and the authorization server's `/.well-known/oauth-authorization-server`, then sends a pushed authorization request (`POST <authorization server>/oauth/par`) with PKCE and a DPoP key thumbprint. The authorize page opens in your system browser (the URL is always shown first; if no browser can launch, the worker tells you to open it manually), and the redirect is received on `http://127.0.0.1:<port>/` (loopback only). The code is exchanged at the authorization server's token endpoint (`POST <authorization server>/oauth/token`), which is also used for refreshes.
- Every request to the authorization server, and every PDS request while OAuth-logged-in, carries `Authorization: DPoP <access token>` plus a `DPoP` proof JWT (ES256, signed by a per-install P-256 key). The client registers as the loopback client `http://localhost?scope=…`; when the Colibri bridge is enabled the scope also contains the `social.colibri.message`/`membership` repo grants and `social.colibri.sync.subscribeEvents?aud=…`, which the PDS requires before it will mint the AppView token. Without the bridge the scope remains `atproto transition:generic`. If your PDS's authorization server does not support the loopback form, set `oauth_client_id` to a hosted copy of `oauth-client-metadata.json`. This repo serves one at `https://ewanc26.github.io/atmospheric-horizon/oauth-client-metadata.json` ([#21](https://github.com/ewanc26/atmospheric-horizon/issues/21)).
- `/atmospheric server-account login <handle>` runs the same OAuth flow for a second account and persists it separately; `/atmospheric server-account publish` writes the server profile as that account. Enable the Colibri bridge before logging in this account so its scope contains the stream grant; a session created earlier must be logged out and authorised again.
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
- **Colibri stream and membership (M26/M27/M32):** the worker lists the server account's `social.colibri.membership` records and creates one for the bridged community when absent; when the bridge starts or the local player logs in, it also repairs the player's own membership the same way. It then opens `wss://<appview>/xrpc/social.colibri.sync.subscribeEvents` with `Sec-WebSocket-Protocol: colibri.auth.bearer, <service-token>`. It sends a heartbeat every 30 seconds, reconnects with backoff, and rotates the token at 45 minutes: the socket closes, the worker mints a fresh token from `com.atproto.server.getServiceAuth` on the server account's PDS (audience `<colibri_appview_did>#colibri_appview`), and reconnects. `/atmospheric colibri reconnect` does the same mint-and-reconnect on demand without repairing membership or re-checking the profile. Only message upserts are used; other event types are ignored. The stream does not send player data beyond the account identity already carried by the token.
- **Colibri server notices (M28):** the server-side `broadcast_message` hook passes join, leave, death and advancement translation keys to the worker. Ordinary chat and unknown lines are ignored. The worker writes the localized line as `social.colibri.message` under the server account, filtered only by `colibri_server_events_enabled`; no player data is added.
- **Colibri presence fallback (M29):** server-side `player_logged_in` and `player_logged_out` hooks schedule a fallback notice. The worker waits 3 seconds and writes it only when no join/leave line, or a blank/hidden line, arrived within 5 seconds of the event. `colibri_server_events_fallback_enabled` (default true) controls the fallback. After a new membership is created, the worker reads the community record's `requiresApprovalToJoin` with an unauthenticated `com.atproto.repo.getRecord`; if true, a HUD status line says a moderator must approve the join. No account mapping or extra player data is added.
- **Colibri community approval read (M29):** the channel's community DID is resolved through its DID document to its PDS, then `com.atproto.repo.getRecord` requests `social.colibri.community` at rkey `self`. This is the only new read added by M29; it runs only after a new membership is created.
- **Colibri profile prompt (M30):** when the bridge starts, the worker reads the local player's own `social.colibri.actor.profile` at rkey `self` with `com.atproto.repo.getRecord`, authenticated as the player. A missing record shows atpcraft's two-line setup prompt with `https://colibri.social`; a failed read is quiet. Other players' profiles are not checked.

### Writes to your PDS

- Notification polling uses `app.bsky.notification.listNotifications` with a `seenAt` timestamp, on a timer and on `/atmospheric notifications`.
- Text and screenshot posts use `com.atproto.repo.uploadBlob` for the image, then `com.atproto.repo.createRecord` with an `app.bsky.embed.images` embed (or a plain text post). The post-location keybind and death messages publish `app.bsky.feed.post` records the same way; death messages only when `post_death_messages` is enabled.
- Chat lines use `com.atproto.repo.createRecord` in the `at.atpcraft.player.chat` collection, TID-keyed — only when `post_chat_messages` is enabled, or one at a time via `/atmospheric chat <text>`. While the Colibri bridge is active, the same chat line is also written to `social.colibri.message` in the player's repo, keyed by a fresh TID; `/atmospheric colibri send <text>` writes one without posting the game chat record. Player-authored Colibri writes are capped at 20 per rolling 60 seconds; the 21st is dropped, server notices are exempt, and the explicit command reports the limit.
- Colibri membership (M27/M32) lists `social.colibri.membership` in the server account's and local player's own repos and creates one `{community, createdAt}` record when the bridged community is absent. A new player membership in an approval-gated community produces atpcraft's pending line; existing memberships and failed writes stay quiet. It uses `createRecord`, not `putRecord`, because the Colibri OAuth grant contains create/delete but not update.
- Snapshots use `com.atproto.repo.createRecord` in `at.atpcraft.player.snapshot`, carrying your position/dimension and an Ed25519 endorsement signature.
- Skins use `com.atproto.repo.uploadBlob` for each PNG, `com.atproto.repo.getRecord` to keep other servers' endorsements, then `com.atproto.repo.putRecord` in `at.atpcraft.player.skin` at the endorsing key id.
- The server profile uses `com.atproto.repo.getRecord` to preserve `createdAt`, then `com.atproto.repo.putRecord` in `at.atpcraft.server.profile` at rkey `self`. `/atmospheric server` writes it as the player account; `/atmospheric server-account publish` writes it as the separate server account (M25).
- Interactions use `com.atproto.repo.createRecord`: `app.bsky.feed.like` or `app.bsky.feed.repost` with a `subject` strong ref `{uri, cid}`, or an `app.bsky.feed.post` with `text`, `createdAt` and `reply: {root, parent}` strong refs.
- Social-graph writes (M23) resolve the subject to a DID, then:
  - `/atmospheric follow` and `/atmospheric block` write `app.bsky.graph.follow` / `app.bsky.graph.block` with `subject` and `createdAt`, keyed by a fresh TID.
  - `/atmospheric unfollow` and `/atmospheric unblock` find matching records in your own repo (the `listRecords` read above) and delete each with `com.atproto.repo.deleteRecord`.
  - `/atmospheric mute` and `/atmospheric unmute` call `app.bsky.graph.muteActor` / `unmuteActor` on the AppView. Mutes are private and create no record. No new data is stored locally.

### Stored on disk

Everything lives in the worker's own data folder — never in a world save. In sidecar mode that is `<game exe dir>/atmospheric-data/`; under Horizon's native storage API it is `horizon-mods/.horizon-mod-data/atmospheric/`.

- `config.json` holds the settings above, including the PDS URL, identifier, app password, poll interval, post flags, chat server id, skin/cape paths and model, server-profile fields, game version, AppView URL and trusted endorsement keys.
- `signing-key.json` holds the Ed25519 seed for snapshot and skin endorsements, generated once.
- `session.json` holds the player's refresh token used to resume across restarts. For OAuth sessions it also holds the DPoP private key (JWK) and token endpoint; app passwords are never stored in it. Delete it or use `/atmospheric logout` to revoke a session.
- `server-session.json` holds the same OAuth material for the separate server account (M25), written owner-only. It is independent of the player session and is deleted by `/atmospheric server-account logout`.
- `cursor.json` (the notification seen-cursor, so restarts do not re-show old notifications) and `snapshot-state.json` (the snapshot version counter, so records never roll back) are not secret. The Colibri bridge stores no additional credential: its service token lives in memory and is minted again on start.
- `skins/` holds skin/cape PNGs downloaded by `/atmospheric skin-of`, content-addressed by blob CID. They are written only for records whose endorsement verified.

The files that hold secrets are written with owner-only permissions (0600 on unix; the OS file ACLs apply on Windows and macOS). The image you post is read from the game's `screenshots/` folder only when you press the post-screenshot keybind; otherwise the latest path stays in memory. Skin/cape PNGs are read from the path you pass (or `skin_path`/`cape_path`) only at that moment; nothing else is read. The login session is held in memory for the running game; the refresh token is persisted so restarts resume automatically unless `persist_session` is off.

### Server-side identity service (M35)

The optional `atmospheric-server` process listens only on `ATMOSPHERIC_SERVER_ADDR` and makes no outbound requests. `GET /healthz` and `GET /v1/links/{uuid}` are public; listing, direct link writes, deletion and pairing creation require `Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN`. Claiming a pairing code is public because the one-time code is the credential. The service stores `links.json` in its own data folder: canonical player UUIDs, player names, DIDs, optional handles, first-link timestamps and pending codes with their expiry. It has no telemetry. The game client does not call this API yet; M35 adds the service and its Docker test only.

### Nothing else

No telemetry, no analytics, no other endpoints. The optional identity service above is a separate listener, not part of the game mod's network use.

## Layout

- `mixins/atmospheric/` — horizon-mixins declarations: the injected std-only `runtime.rs` (which also spawns and bridges the sidecar), its placeholder hook, and `horizon-mixin.toml`.
- `crates/atmospheric/` — the protocol library: `config`, `session`, `oauth` (M8), `actions`, `notifications`, `lists` (M11/M15), `labels` (M12), `presence` (M13), `profiles` (M24), `skins` (M14), `chats` (M16), `servers` (M19), `graph` (M20 reads/M23 writes), `interactions` (M21), `records` (atpcraft lexicon types), `tid`, `signing` (Ed25519 endorsements) and `worker`, plus the `atmospheric-worker` bin and its end-to-end test (`tests/sidecar.rs`).
- `crates/atmospheric-server/` — the optional HTTP identity service: link model and validation, atomically persisted store, admin-auth router and integration tests.
- `docker/` — the server image, compose file and end-to-end Docker test.
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
