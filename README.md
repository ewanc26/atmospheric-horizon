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

**v0.3.1** (14 November 2026) is the current release. The mod is usable in-game today through the `atmospheric-worker` sidecar: the injected runtime is std-only, and every network or disk operation happens in the worker. Public reads work signed out; posting and record writes need a Bluesky account.

Shipped in v0.3.1: OAuth and app-password login, text a
nd screenshot posts, notifications, atpcraft-compatible chat, signed snapshots and skins, a server profile, read sides for lists, labels, presence, skins, chat history and server profiles, social-graph reads and writes, reply/like/repost, profile reads, a separate server account, the Colibri chat bridge and its follow-ups, the optional server-side identity service with in-game and browser linking, and the online whitelist sweep (M23–M42).

Not yet: launcher registry distribution, which waits on third-party native-mod support upstream ([#28](https://github.com/ewanc26/atmospheric-horizon/issues/28)); automatic cross-server player-data sync and core-NBT restore. M56–M64 add signed-label encoding, owner-only key persistence, the durable label log, the public `queryLabels` and `subscribeLabels` endpoints, the server-account service declaration, its PLC identity publication (`#atproto_label` and `#atproto_labeler`), the game triggers for the automatic label vocabulary, manual self-service capture/restore, RPG item discovery, and browser skin upload. The separate server account (M25), the Colibri bridge (M26–M34) and the shared UUID-to-DID map (M35–M42) are the foundations those milestones build on.

## Features

- **Post from the game.** Publish text with `/atmospheric post`, or the latest F2 screenshot or your current position from a keybind, plus optional death messages.
- **Join the conversation.** Reply to, like or repost any Bluesky post, from its AT-URI or its bsky.app URL.
- **Notifications in the HUD.** A digest line, a poll timer and a cursor that survives restarts.
- **atpcraft-compatible records.** Chat messages, Ed25519-endorsed snapshots and skins with blob uploads, and a server descriptor at rkey `self`.
- **Edit your profile.** `/atmospheric profile-set <name|description|pronouns|avatar|banner> [text|path]` updates the logged-in account's Bluesky profile. Empty text clears a text field; avatar and banner accept a local PNG/JPEG up to 1 MB. Other fields are
 preserved (M65/M66).
- **Upload a skin from the browser.** `/atmospheric skin upload` gives an already-linked player a one-time browser link. After Bluesky sign-in, the identity service uploads the PNG and publishes the same Ed25519-endorsed `at.atpcraft.player.skin` record the in-game writer produces, preserving the cape and other servers' endorsements (M64). In-game application still waits on a native Horizon skin setter ([#113](https://github.com/ewanc26/atmospheric-horizon/issues/113)).
- **Read the public side.** List membership (with a fail-closed gate decision), labels and labeler queries, player presence, skins, chat history, arbitrary public collections (M44), server profiles, profiles with display names and pronouns, and follows/followers/blocks/mutes.
- **Serve labels.** The optional identity service can sign and store labels for player DIDs, serve the spec `com.atproto.label.queryLabels` endpoint and stream the log over `subscribeLabels`; operators append labels through the admin token. M56–M62 cover signing, storage, querying, live streaming, the server-account declaration, its PLC identity publication and the game triggers for the full automatic vocabulary: `player`, `online`, `admin`, `playtime-bronze`/`silver`/`gold`, `dragon-slayer`, `monster-hunter` and `death-defier`. `vip` and `donor` remain manual grants.
- **Show linked players' Bluesky names.** On a server, a joining player linked through the identity service gets their Bluesky display name reduced to atpcraft's valid 16-character Minecraft name, with the handle's username as the fallback, plus pronouns as their in-game name — plain text in chat and the player list (M45/M47). Unlinked players and unusable names keep their Minecraft name.
- **Publish the schemas.** The optional identity service serves the five `at.atpcraft.*` lexicon documents at `/lexicons`, so other clients and servers can fetch the record shapes this mod writes (M46).
- **Shape your social graph.** Follow, unfollow, block, 
unblock, mute or unmute an account from its DID or handle.
- **Sign in properly.** OAuth in the system browser (PAR + PKCE + DPoP, loopback redirect), with app passwords as the fallback.
- **Run a server account.** `/atmospheric server-account login <handle>` gives the server its own persisted OAuth identity, separate from the player, and can publish the server descriptor from that account.
- **Link players across the server.** The optional `atmospheric-server` process maps canonical Minecraft UUIDs to ATProto DIDs. A player runs `/atmospheric link` in game, opens the one-time URL, and signs in with Bluesky; the OAuth result binds the UUID the game server supplied, not a UUID typed into the browser. `/atmospheric link <code>` still claims an operator-issued code. A linked player who joins without a Colibri profile gets the setup prompt directly. With `join_gate_enabled`, the game server resolves each joining UUID and refuses a linked DID that is not on `join_gate_list`; unlinked players and failed verification are refused too. When an unlinked player is refused and the shared link token is configured, the disconnect reason includes a one-time browser URL; signing in there binds the UUID and the player can reconnect. A periodic sweep (M42, `join_gate_sweep_seconds`) checks the list and disconnects an online player whose DID has left it, so a list edit takes effect without waiting for a reconnect; list members are cached for `list_cache_seconds` (M52, default 300) and refreshed when stale or when a gate check misses. Links survive restarts in a local JSON document; browser link sessions do not.
- **Bridge a Colibri channel.** With `colibri_bridge_enabled`, the server account declares its community membership, opens the Colibri event stream and rotates its token before expiry; channel messages appear in game chat, game chat is published into the channel as your player account, and join/leave/death/advancement lines go out as the server account. Hidden or rewritten join
/leave lines fall back to the connection event, and a new membership that needs moderator approval gets a HUD notice. The stream can be reconnected without dropping the bridge, and player messages are capped at 20 per rolling minute. The local player's own community membership is repaired when the bridge starts or they log in, and a new gated join gets atpcraft's pending-approval line. When another linked player joins, the worker resolves their DID through the identity service and prompts them directly if their own Colibri profile is missing.
- **Keep I/O off the game thread.** All network and disk I/O lives on one worker thread; the game thread exchanges JSON lines with it. The only intentional wait is the M38 login hook, bounded to 3 seconds for gate state and 6 seconds for the decision.
- **No surprises.** Writes go to your own PDS and public reads go to the configured AppView or the subject's PDS. The only other requests are the identity link claim, the join-time link lookup against the server URL you configure, the linked player's public profile read, and the M61/M62 labeler trigger's link lookup, active-label query and signed emission against that same service.

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

1. Download the archive for your OS from [Releases](https://github.com/ewanc26/atmospheric-horizon/
releases) — `atmospheric-worker-vX.Y.Z-linux-x86_64.tar.gz`, `...-macos-aarch64.tar.gz` or `...-windows-x86_64.zip` — plus `atmospheric-horizon-mixins-vX.Y.Z.tar.gz` and `SHA256SUMS`. Check the hash.
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
| `chat <text>` 
| Publish one `at.atpcraft.player.chat` record |
| `snapshot` | Publish a signed player-state snapshot |
| `player-data capture [player]` | Capture aspects into a signed snapshot (linked self only) |
| `player-data restore [player]` | Verify and restore the latest snapshot's aspects |
| `rpg discover [player]` | Publish RPG items found in statistics and renamed stacks |
| `skin [path] [default\|slim]` | Publish a skin/cape record and its blobs |
| `skin upload` | Open a one-time browser link to upload and publish a skin (linked players, M64) |
| `server` | Publish or refresh the server profile from the player account |
| `server-account login <handle>\|status\|logout\|publish` | Manage the separate server account and publish the server profile from it |
| `colibri start\|stop\|reconnect\|status\|send <text>` | Start, stop, reconnect, inspect or write to the Colibri chat bridge |
| `link [code]` | Start browser linking, or claim a server identity pairing code |
| `server-of <did\|handle>` | Read and verify a server profile |
| `profile <did\|handle>` | Read a Bluesky profile and its pronouns |
| `profile-set <name\|description\|pronouns\|avatar\|banner> [text\|path]` | Update profile text or upload an avatar/banner; empty text clears (M65/M66) |
| `list <at-uri\|bsky.app URL>` | List the members of a Bluesky list |
| `gate <list> <did\|handle>` | Decide list membership, fail-closed |
| `labels <subject>` | Read labels from the AppView |
| `labeler <did> [subject]` | Query a labeler directly |
| `player <did\|handle>` | Read a player's latest verified snapshot |
| `skin-of <did\|handle>` | Read a verified skin and download its blobs |
| `chat-of <did\|handle>` | Read a player's mirrored chat history |
| `records <did\|handle> <collection>` | Browse any public collection on a PDS |
| `follows\|followers\|blocks\|mutes <did\|handle>` | Read a public social graph |
| `follow\|unfollow\|block\|unblock\|mute\|unmute <did\|handle>` | Write one relationship |
| `reply <at-ur
i> <text>` | Reply to a post |
| `like <at-uri>` | Like a post |
| `repost <at-uri>` | Repost a post |

`bsky.app` post URLs work wherever an `at-uri` is accepted. The `ATMOSPHERIC_*` probes in [`examples/pds_check.rs`](crates/atmospheric/examples/pds_check.rs) exercise the network paths without starting the game.

## Configuration

`config.json` lives in the worker's data folder. The defaults are:

| Key | Default | Purpose |
| --- | --- | --- |
| `pds_url` | `https://bsky.social` | PDS to read from and write to |
| `appview_url` | `https://public.api.bsky.app` | AppView for public reads |
| `server_api_url` | empty | Base URL of the optional M35 identity service for `/atmospheric link`, the join gate, the M61/M62 labeler triggers and the M63 self-service link check |
| `server_link_token` | empty | Server-only shared secret for browser-link creation; must match `ATMOSPHERIC_SERVER_LINK_TOKEN` |
| `server_admin_token` | empty | Identity service admin bearer token for the M61/M62 labeler triggers; must match `ATMOSPHERIC_SERVER_TOKEN` and is a server-only secret |
| `browser_link_ttl_seconds` | `600` | Lifetime of a one-time browser link in seconds; the service accepts 60–3600 (M55) |
| `join_gate_enabled` | `false` | Refuse joins at login unless the player is linked and on `join_gate_list` (M38) |
| `join_gate_list` | empty | Public Bluesky list AT-URI or bsky.app URL used by the join gate; empty fails closed when the gate is on |
| `join_gate_sweep_seconds` | `60` | Seconds between online whitelist sweeps; `0` disables it, and a positive value is floored at 60 (M42) |
| `list_cache_seconds` | `300` | Seconds to cache list members; `0` refreshes on every read (M52) |
| `identifier` | empty | Handle, email or DID for app-password login |
| `app_password` | empty | App password — never an account password |
| `poll_interval_secs` | `60` | Seconds between notification polls |
| `server_account_token_refresh_seconds` | `60` | Seconds between proactive server-account to
ken checks; a healthy token stays an in-memory check (M50) |
| `labeler_enabled` | `false` | Publish the declaration on login and startup resume, request `identity:*` for the PLC identity update, and arm the game join/logout/sweep label triggers (M59–M62) |
| `persist_session` | `true` | Keep the refresh token in `session.json` across restarts |
| `post_death_messages` | `false` | Post a text post when you die |
| `post_chat_messages` | `false` | Mirror every chat line to your repo |
| `chat_server_id` | empty | Optional override for the chat record's `server` field; empty uses the signing key id, matching atpcraft |
| `skin_path`, `cape_path` | empty | Default PNGs for `/atmospheric skin` |
| `skin_model` | `default` | Skin model: `default` or `slim` |
| `oauth_client_id` | empty | Hosted client-metadata URL when loopback registration is unsupported |
| `oauth_scope` | empty | Base OAuth scope; empty uses `atproto transition:generic`. A narrow base gains the snapshot/skin/chat/blob grants the implemented writes need (M54), the `equipment.rpg.item` grant when RPG discovery is on, and Colibri grants when the bridge is on |
| `rpg_items_enabled` | false | Publish discovered RPG items as an `equipment.rpg.give`/`equipment.rpg.item` pair; needs the server account and a linked player. Changing it requires a fresh OAuth login so the scope includes the RPG grants |
| `rpg_items_max_per_scan` | 10 | Maximum new RPG records written per scan |
| `client_name` | `atmospheric-horizon` | Fallback `name` for the server profile when `server_name` is blank |
| `server_name`, `server_description`, `server_address`, `server_website`, `server_contact` | empty | Server-profile identity; blank omits the field, and `server_name` falls back to `client_name` |
| `server_capabilities` | empty | Optional override; empty derives `join-gate` when armed, `skin-sync` and `labeler` when enabled |
| `game_version` | `26.3` | `minecraftVersion` in the server profile |
| `trusted_server_keys` | `[]`
 | `{key_id, public_key}` endorsements accepted on reads |
| `colibri_bridge_enabled` | `false` | Arm the bridge after server-account login |
| `colibri_channel_uri` | empty | Channel AT-URI or Colibri app link |
| `colibri_appview_url` | `https://api.colibri.social` | Colibri AppView base URL |
| `colibri_appview_did` | `did:web:api.colibri.social` | AppView service DID; `#colibri_appview` is appended for the audience |
| `colibri_server_events_enabled` | `true` | Relay server join/leave/death/advancement lines into the channel |
| `colibri_server_events_fallback_enabled` | `true` | Fall back to the connection event when a chat mod hides/rewrites a join or leave line |

The app password, `server_link_token` and `server_admin_token` are the only secrets in `config.json`; OAuth sessions never store one.

## Server-side identity service

The optional `atmospheric-server` process supplies the shared Minecraft UUID → ATProto DID map that a client cannot build on its own. It is a separate HTTP service, not part of the game process. Set `server_api_url` to its base URL and configure `server_link_token` with the service's `ATMOSPHERIC_SERVER_LINK_TOKEN`. A player then runs `/atmospheric link` in game, gets a one-time URL in chat, and signs in with Bluesky in the browser. The game server creates that link token over an authenticated request, so the browser never chooses or supplies the UUID. The URL's lifetime is `browser_link_ttl_seconds` (default 600 seconds, accepted range 60–3600). `/atmospheric link <code>` still accepts an operator-issued M35 pairing code. With `join_gate_enabled` on, the server-side login hook resolves each joining UUID through this map and reads the configured public list before the player is placed. An unlinked player, a DID not on the list, or a failed read is refused. An unlinked player refused by an armed gate gets a one-time browser URL on the disconnect screen when `server_link_token` is set, so they can link and reconnect; if link creation fa
ils, the generic refusal still fails closed.

Browser linking needs a public base URL reachable by the authorization server and the player's browser. OAuth requests go from this service to the handle resolver, the account's PDS, its authorization server and the account's PDS again; it does not contact the AppView for linking. Those are the service's only outbound requests. The game worker also calls this service's `POST /v1/browser/pairings` from `/atmospheric link` and from the gate refusal, so `server_link_token` must match on both sides.

Browser skin upload reuses the OAuth client with a separate `/skin/callback` redirect. Set `ATMOSPHERIC_SERVER_SKIN_KEY` to the game worker's `signing-key.json` (or a copy) to enable it. `/atmospheric skin upload` asks `POST /v1/browser/skins` for a one-time link only after the player's UUID is already linked; the page accepts a PNG up to 1 MiB, signs the browser in, requires the same DID, uploads the blob and upserts the existing `at.atpcraft.player.skin` record with the worker's Ed25519 endorsement. The link is single-use and uses the browser-link lifetime. Tokens, verifiers, DPoP keys and uploaded bytes stay in memory. The cape and other servers' endorsements are preserved. In-game application is not implemented because the pinned Horizon API has no skin/texture setter ([#113](https://github.com/ewanc26/atmospheric-horizon/issues/113)).

The same listener can act as a labeler. Set `ATMOSPHERIC_SERVER_LABELER_DID` to the server account's `did:plc` (or `did:web`) to enable it. The service creates an owner-only K-256 signing key at `<data>/labeler/signing-key.json` and appends signed labels to `labeler/labels.jsonl`; `POST /v1/labeler/labels` signs and stores one label with the admin token, and `GET /xrpc/com.atproto.label.queryLabels` serves active labels publicly. `GET /xrpc/com.atproto.label.subscribeLabels` is the spec WebSocket: it backfills the durable log from `cursor` (or the full history when omitted/0) and then streams
 new labels; a cursor past the tail gets a `FutureCursor` error frame and a normal close. Both public XRPC reads answer permissive CORS for browser consumers. Labels carry the subject DID/AT-URI and value, not player names or game data. The labeler HTTP service itself writes no declaration. With `labeler_enabled` on, the worker publishes `app.bsky.labeler.service` as the server account through its OAuth session (M59). `GET /v1/labeler/identity` reports the service DID, its `did:key` signing key and its public endpoint; `/atmospheric labeler-identity request` emails a one-time PLC token and `/atmospheric labeler-identity submit <token>` publishes `#atproto_label` and `#atproto_labeler` in the account's DID document while preserving every other entry (M60). `status` reports what the document currently has. With `labeler_enabled` and `server_admin_token` set, the worker also triggers the activity labels from game events (M61/M62). A linked player's join forces the next tick's sweep, which captures their op level and vanilla statistics and asserts `player` (30-day expiry, refreshed at most once a day), `online`, `admin` (op level at least 1), the highest playtime tier (10/100/1000 hours), `dragon-slayer` (one Ender Dragon kill), `monster-hunter` (1000 mob kills) and `death-defier` (100 deaths). Labels the current state no longer justifies are negated, except `vip` and `donor`, which are manual grants and are never touched. A disconnect negates `online`; a once-a-minute sweep re-evaluates online players; and worker startup negates `online` labels left by an unclean stop. The capture stays in the game and worker; only the subject DID and label values reach the labeler. Unlinked players and failed lookups are skipped, and every automatic trigger stays quiet on error.

| Variable | Default | Purpose |
| --- | --- | --- |
| `ATMOSPHERIC_SERVER_ADDR` | `0.0.0.0:8787` | Listen address |
| `ATMOSPHERIC_SERVER_DATA_DIR` | `./atmospheric-server-data` | Folder containing `links.js
on` and the labeler data |
| `ATMOSPHERIC_SERVER_TOKEN` | unset | Admin bearer token; unset disables all admin routes and pairing creation |
| `ATMOSPHERIC_SERVER_LINK_TOKEN` | unset | Shared secret the game server uses to create browser links; unset disables browser linking |
| `ATMOSPHERIC_SERVER_PUBLIC_URL` | unset | Public HTTPS base URL used for OAuth client metadata and the callback |
| `ATMOSPHERIC_SERVER_LABELER_DID` | unset | Server account `did:plc`/`did:web` to sign labels as; unset disables label emission and querying |
| `ATMOSPHERIC_SERVER_SKIN_KEY` | unset | Path to the game worker's `signing-key.json` for browser skin publishing; unset disables uploads |

### Docker setup example

The repository includes a copyable environment template at `docker/.env.example`. Put a real public HTTPS URL in `ATMOSPHERIC_SERVER_PUBLIC_URL`; it must route to the container and terminate TLS, and its `/link`, `/skin` and `/oauth-client-metadata.json` paths must reach port 8787. To enable browser skin upload, copy the worker's `signing-key.json` into the data volume and set `ATMOSPHERIC_SERVER_SKIN_KEY=/data/skin-signing-key.json`. The link token is shared with the game server and is never sent to a browser.

```bash
cp docker/.env.example docker/.env
# edit docker/.env: set both tokens and the public HTTPS URL
docker compose --env-file docker/.env -f docker/compose.yaml up -d --build
```

Add these keys to the game's `config.json` (other settings keep their defaults):

```json
{
  "server_api_url": "https://links.example.com",
  "server_link_token": "the-same-value-as-ATMOSPHERIC_SERVER_LINK_TOKEN",
  "server_admin_token": "the-same-value-as-ATMOSPHERIC_SERVER_TOKEN",
  "join_gate_enabled": true,
  "join_gate_list": "at://did:plc:owner/app.bsky.graph.list/your-list",
  "join_gate_sweep_seconds": 60,
  "list_cache_seconds": 300
}
```

With the gate on, an unlinked player is refused with a browser URL in the disconnect reason. They open it, sign in with Bluesky, then reco
nnect. The same flow is available to an already-connected player through `/atmospheric link`. If `ATMOSPHERIC_SERVER_LINK_TOKEN` or `ATMOSPHERIC_SERVER_PUBLIC_URL` is unset, browser linking is disabled and the gate stays fail-closed.

The sweep closes the gap a list edit leaves behind: every `join_gate_sweep_seconds` the server checks `join_gate_list` and disconnects a linked player whose DID has left it (atpcraft's online whitelist sweep). Members are cached for `list_cache_seconds` (M52, default 300); a stale cache refreshes once, and a fresh cache hit costs no AppView request. A failed list read disconnects nobody, and a per-player link lookup that fails skips that player, so a transient error never kicks someone by accident. Set `join_gate_sweep_seconds` to `0` to leave the gate check at login only.

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | public | Status, version, link count and live pairing count |
| `GET /v1/links/{uuid}` | public | Resolve one player's link |
| `GET /v1/links` | admin | List every link |
| `PUT /v1/links/{uuid}` | admin | Create or replace a link directly |
| `DELETE /v1/links/{uuid}` | admin | Remove a link |
| `POST /v1/pairings` | admin | Issue a one-time pairing code for a player |
| `POST /v1/pairings/claim` | pairing code | Bind the code's player to a DID and handle |
| `POST /v1/browser/pairings` | link token | Create a one-time browser link for an online player (`ttl_seconds` optional, 60–3600) |
| `POST /v1/browser/skins` | link token | Create a one-time skin-upload link for an already-linked player |
| `POST /v1/labeler/labels` | admin | Sign and append one label for a subject (`uri`, `val`, optional `neg`/`cid`/`exp`) |
| `GET /v1/labeler/identity` | public | Labeler DID, signing `did:key` and public endpoint for the PLC identity update (M60) |
| `GET /xrpc/com.atproto.label.queryLabels` | public | Query active signed labels (`uriPatterns` required; `sources`, `limit`, `cursor`) |
| `GET /xrpc/com.atpr
oto.label.subscribeLabels` | public (WebSocket) | Backfill from `cursor` and stream signed labels; `FutureCursor` past the tail |
| `GET /oauth-client-metadata.json` | public | OAuth client metadata, at the URL used as the client id |
| `GET /lexicons` | public | HTML index of the five published `at.atpcraft.*` lexicons (M46) |
| `GET /lexicons/{nsid}.json` | public | One bundled lexicon document (M46) |
| `GET /link/{token}` | link token | Browser sign-in form |
| `POST /link/{token}` | link token | Start the Bluesky authorization request |
| `GET /link/callback` | OAuth state | Finish sign-in and save the link |
| `GET /skin/{token}` | upload token | Browser skin upload form |
| `POST /skin/{token}` | upload token | Validate the PNG and start Bluesky sign-in |
| `GET /skin/callback` | OAuth state | Finish sign-in, upload the blob and publish the signed record |

Direct operator write:

```bash
curl -X PUT http://127.0.0.1:8787/v1/links/00000000-0000-4000-8000-000000000001 \
  -H "Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"player_name":"Alex","did":"did:plc:...","handle":"alex.example"}'
```

Label example:

```bash
curl -X POST http://127.0.0.1:8787/v1/labeler/labels \
  -H "Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"uri":"did:plc:...","val":"online"}'

curl 'http://127.0.0.1:8787/xrpc/com.atproto.label.queryLabels?uriPatterns=did%3Aplc%3A...'
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

In game, run `/atmospheri
c link ABCD-EFGH` after setting `server_api_url` to the service base. Codes are one-time, default to ten minutes and accept 60–3600 seconds. Reissuing a code for the same UUID invalidates the previous one. Links and pending codes are written through an owner-only temporary file and an atomic rename on Unix (the Docker target) to `links.json`; a failed write leaves the old state in memory and on disk. Run the service in Docker with `docker compose --env-file docker/.env -f docker/compose.yaml up -d --build` after copying and editing `docker/.env.example`; `docker/.env.example` shows the required admin token, link token and public URL, plus the optional labeler DID. Run the end-to-end test with `bash docker/server-test.sh`.

Browser flow: run `/atmospheric link` with no code. The worker posts `{player_uuid, player_name, ttl_seconds}` to `/v1/browser/pairings` with the link token, receives a URL, and tells only that player to open it. The same request runs from a gate refusal for an unlinked player, with the URL on the disconnect screen. The page resolves the handle, pushes a PAR request, redirects to Bluesky, and exchanges the callback with PKCE and DPoP. The callback binds the signed-in DID to the UUID the game supplied. The link's lifetime is `browser_link_ttl_seconds` (default ten minutes, at most one hour). OAuth tokens and verifiers stay in memory, single-use, expire after ten minutes, and a service restart invalidates them.

`/atmospheric skin upload` uses the same browser flow with its own `/skin` routes. The service checks the player is already linked, takes a PNG up to 1 MiB, requires the signed-in DID to match that link, uploads `com.atproto.repo.uploadBlob`, and upserts `at.atpcraft.player.skin` at rkey = signing key id with the configured key's endorsement. The cape and other servers' endorsements are preserved; tokens, upload bytes and verifiers stay in memory, and the upload link is consumed after a successful publish.

M35 supplies the mapping, M36 lets
 a player claim a link in game, M37 checks a linked player's Colibri profile when they join, M38 enforces the configured list at login for linked accounts, M39 completes the browser flow, M40 puts a fresh browser link on an unlinked player's gate refusal, and M42 sweeps online players so a list edit takes effect without a reconnect. Labeler signing, durable storage, the public query endpoint, the live stream, the server-account declaration and its PLC identity publication are M56–M60; M61/M62 add the game triggers and the op-level/statistic labels. M63 adds manual, link-gated aspect capture/restore and RPG discovery, and M64 adds browser skin upload; automatic cross-server sync, core-NBT restore and in-game skin application remain future work

... [Content truncated]

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow and the checks required before opening a pull request.

## Licence

See [LICENSE](LICENSE) for the licence terms.
