<p align="center">
  <a href="https://github.com/ewanc26/atmospheric-horizon/actions/workflows/ci.yml"><img src="https://github.com/ewanc26/atmospheric-horizon/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/ewanc26/atmospheric-horizon/releases/latest"><img src="https://img.shields.io/github/v/release/ewanc26/atmospheric-horizon?label=release" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/ewanc26/atmospheric-horizon?label=licence" alt="AGPL-3.0"></a>
  <a href="https://github.com/sponsors/ewanc26"><img src="https://img.shields.io/github/sponsors/ewanc26?logo=githubsponsors&logoColor=white&label=sponsors" alt="Sponsor"></a>
</p>

# atmospheric-horizon

AT Protocol integration for [Horizon](https://playhorizon.dev), the Rust rewrite of Minecraft: Java Edition — post text and screenshots to Bluesky, Spark or Grain, reply and react, and read notifications, profiles and public graph data without leaving the game.

> Independent project; not affiliated with Mojang, Microsoft, Bluesky PBC, or the Horizon project. NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

This is the successor to [jollywhoppers-socialsync](https://github.com/ewanc26/jollywhoppers-socialsync), the Paper plugin that linked Minecraft players to AT Protocol identities, rebuilt as a native Horizon mod. It also ports the feature set of [atpcraft](https://git.atforge.org/atforge/atpcraft/ATPcraft) client-side; its lexicons are reimplemented as data, and no code is copied because the upstream repo carries no licence. See [Roadmap](#roadmap).

## Status

**v0.11.0** (25 November 2026) is the current release. The mod is usable in-game today through the `atmospheric-worker` sidecar: the injected runtime is std-only, and every network or disk operation happens in the worker. Public reads work signed out; posting and record writes need an account on the configured PDS.

Shipped in v0.11.0: Operator's Warrant — experimental DID-authenticated, short-lived scoped admin grants for the optional identity sidecar with signed audit receipts (#189), on top of v0.10.0's verified handle plaques — cryptographic handle verification from a fresh signed commit (#185), then v0.9.0's cosmetic Warrants — signed ranks from pinned issuer records (#192), v0.8.0's Counting Stones — in-game polls with signed Merkle-root tallies and private voter receipts (#193), v0.7.0's configurable Bluesky/Spark/Grain image posts (#183), v0.6.0's achievement announcements and automatic F2 screenshot posting (#181/#182), v0.5.0's automatic player-data sync, liveness-checked server cards and current-major crypto dependencies with unchanged wire formats, itself everything in v0.4.0 — itself everything in v0.3.1 (OAuth and app-password login, text and screenshot posts, notifications, atpcraft-compatible chat, signed snapshots and skins, a server profile, record/label/presence/chat/profile reads, social-graph reads and writes, reply/like/repost, a separate server account, the Colibri bridge, the server-side identity service with in-game and browser linking, and the online whitelist sweep) plus generic record browsing, linked-player names, hosted lexicons, proactive token refresh and automatic server-profile publish, a list-member cache, configurable OAuth scope, grants and browser-link lifetime, a complete labeler, player-data aspect capture and RPG item discovery, browser skin upload, and profile editing with avatar and banner uploads (M23–M66).

Not yet: launcher registry submission, which is no longer blocked by upstream — current Horizon supports third-party native mods and a trusted native server export, per ADR-002 ([#209](https://github.com/ewanc26/atmospheric-horizon/issues/209)); the native server half that uses that export for server-DID egress; full core-NBT restore, which needs a complete round-trip player-compound setter the pinned Horizon native API does not expose; in-game skin application, which needs a texture-signing/relay helper the pinned Horizon native API does not expose ([#113](https://github.com/ewanc26/atmospheric-horizon/issues/113)). The separate server account (M25), the Colibri bridge (M26–M34), the shared UUID-to-DID map (M35–M42), the labeler (M56–M62) and the profile/identity services (M63–M66) are the foundations those milestones build on.

## Features

- **Post from the game.** Publish text with `/atmospheric post`, or the latest F2 screenshot or your current position from a keybind, plus optional death messages, advancement announcements (#181) and automatic screenshot posting (#182). Screenshots can target Bluesky (`app.bsky.feed.post`), Spark (`so.sprk.feed.post`) or Grain (`social.grain.photo` plus a gallery) with `image_post_target` (#183).
- **Join the conversation.** Reply to, like or repost any Bluesky post, from its AT-URI or its bsky.app URL.
- **Notifications in the HUD.** A digest line, a poll timer and a cursor that survives restarts.
- **atpcraft-compatible records.** Chat messages, Ed25519-endorsed snapshots and skins with blob uploads, and a server descriptor at rkey `self`.
- **Automatic player-data sync (M68).** With `player_data_sync_enabled`, a linked player's inventory, ender chest, equipment, advancements and statistics are captured when they leave and the newest verified snapshot is restored when they join — on this server or a trusted peer server. The manual `/atmospheric player-data` commands keep working either way. Full player-NBT restore remains unavailable because Horizon has no complete round-trip setter for the whole player compound.
- **Edit your profile.** `/atmospheric profile-set <name|description|pronouns|avatar|banner> [text|path]` updates the logged-in account's Bluesky profile. Empty text clears a text field; avatar and banner accept a local PNG/JPEG up to 1 MB. Other fields are preserved (M65/M66).
- **Upload a skin from the browser.** `/atmospheric skin upload` gives an already-linked player a one-time browser link. After Bluesky sign-in, the identity service uploads the PNG and publishes the same Ed25519-endorsed `at.atpcraft.player.skin` record the in-game writer produces, preserving the cape and other servers' endorsements (M64). In-game application still waits on a native Horizon skin setter ([#113](https://github.com/ewanc26/atmospheric-horizon/issues/113)).
- **Read the public side.** List membership (with a fail-closed gate decision), labels and labeler queries, player presence, skins, chat history, arbitrary public collections (M44), server profiles, profiles with display names and pronouns, and follows/followers/blocks/mutes.
- **Warrants: cosmetic ranks (#192).** `/atmospheric warrants <did|handle>` reads signed credential records from a player's own repo and shows the ones issued by a DID, collection and Ed25519 key you pin in `warrant_issuers`. A self-attested rank is worthless: the signature must verify with the exact pinned key, for that subject, while the grant is active. Deleting the record drops the rank on the next fetch and `expiresAt` can time-box it. Ranks are name-plate text only — never a join gate or gameplay capability, and nothing is written to the PDS.
- **Verified handle plaques (#185).** `/atmospheric verify <did|handle>` proves a handle belongs to the logged-in account cryptographically: the worker writes a throwaway nonce record through your own PDS session to force a fresh commit, then verifies that commit's signature against the `#atproto` key in your PLC document, checks the commit is newer than the challenge, and confirms the handle resolves back to the same DID. The plaque states plainly that this proves PDS repository custody, **not** human identity. The subject must be the logged-in account; no profile data is written.
- **Operator's Warrant (#189, experimental).** The optional `atmospheric-server` sidecar can grant short-lived, scoped admin access to allowlisted DIDs that prove PDS repository custody with the same PLC-anchored commit check as verified handle plaques (#185). DID grants are **additive**: the existing `ATMOSPHERIC_SERVER_TOKEN` stays valid and unchanged on every admin route. This is operators only — no in-game join gate, label trigger or other player-facing state reads it, and it proves repository custody, **not** human identity. See [DID-authenticated operator grants](#did-authenticated-operator-grants-experimental) below.
- **Serve labels.** The optional identity service can sign and store labels for player DIDs, serve the spec `com.atproto.label.queryLabels` endpoint and stream the log over `subscribeLabels`; operators append labels through the admin token. M56–M62 cover signing, storage, querying, live streaming, the server-account declaration, its PLC identity publication and the game triggers for the full automatic vocabulary: `player`, `online`, `admin`, `playtime-bronze`/`silver`/`gold`, `dragon-slayer`, `monster-hunter` and `death-defier`. `vip` and `donor` remain manual grants.
- **Show linked players' Bluesky names.** On a server, a joining player linked through the identity service gets their Bluesky display name reduced to atpcraft's valid 16-character Minecraft name, with the handle's username as the fallback, plus pronouns as their in-game name — plain text in chat and the player list (M45/M47). Unlinked players and unusable names keep their Minecraft name.
- **Publish the schemas.** The optional identity service serves the five `at.atpcraft.*` lexicon documents at `/lexicons`, so other clients and servers can fetch the record shapes this mod writes (M46).
- **Shape your social graph.** Follow, unfollow, block, unblock, mute or unmute an account from its DID or handle.
- **Sign in properly.** OAuth in the system browser (PAR + PKCE + DPoP, loopback redirect), with app passwords as the fallback.
- **Run a server account.** `/atmospheric server-account login <handle>` gives the server its own persisted OAuth identity, separate from the player, and can publish the server descriptor from that account.
- **Link players across the server.** The optional `atmospheric-server` process maps canonical Minecraft UUIDs to ATProto DIDs. A player runs `/atmospheric link` in game, opens the one-time URL, and signs in with Bluesky; the OAuth result binds the UUID the game server supplied, not a UUID typed into the browser. `/atmospheric link <code>` still claims an operator-issued code. A linked player who joins without a Colibri profile gets the setup prompt directly. With `join_gate_enabled`, the game server resolves each joining UUID and refuses a linked DID that is not on `join_gate_list`; unlinked players and failed verification are refused too. When an unlinked player is refused and the shared link token is configured, the disconnect reason includes a one-time browser URL; signing in there binds the UUID and the player can reconnect. A periodic sweep (M42, `join_gate_sweep_seconds`) checks the list and disconnects an online player whose DID has left it, so a list edit takes effect without waiting for a reconnect; list members are cached for `list_cache_seconds` (M52, default 300) and refreshed when stale or when a gate check misses. Links survive restarts in a local JSON document; browser link sessions do not.
- **Bridge a Colibri channel.** With `colibri_bridge_enabled`, the server account declares its community membership, opens the Colibri event stream and rotates its token before expiry; channel messages appear in game chat, game chat is published into the channel as your player account, and join/leave/death/advancement lines go out as the server account. Hidden or rewritten join/leave lines fall back to the connection event, and a new membership that needs moderator approval gets a HUD notice. The stream can be reconnected without dropping the bridge, and player messages are capped at 20 per rolling minute. The local player's own community membership is repaired when the bridge starts or they log in, and a new gated join gets atpcraft's pending-approval line. When another linked player joins, the worker resolves their DID through the identity service and prompts them directly if their own Colibri profile is missing.
- **Keep I/O off the game thread.** All network and disk I/O lives on one worker thread; the game thread exchanges JSON lines with it. The only intentional wait is the M38 login hook, bounded to 3 seconds for gate state and 6 seconds for the decision.
- **No surprises.** Writes go to your own PDS and public reads go to the configured AppView or the subject's PDS. The only other requests are the identity link claim, the join-time link lookup against the server URL you configure, the linked player's public profile read, and the M61/M62 labeler trigger's link lookup, active-label query and signed emission against that same service.

## Requirements

| | |
| --- | --- |
| Rust | 1.95+ (pinned by `rust-toolchain.toml`) |
| Game | Minecraft: Java Edition 26.3 through the official launcher |
| Account | An AT Protocol account on the configured PDS, such as Bluesky (OAuth, or an app password as the fallback) |
| OS | Linux for native staging; the worker is released for Linux x86_64, macOS arm64 and Windows x86_64 |
| Docker | Optional, for the server-side identity service and its end-to-end test |

## Install

The launcher supports third-party native mods and a trusted standalone dedicated-server export (ADR-002, [#209](https://github.com/ewanc26/atmospheric-horizon/issues/209)); native mods are built against the pinned revisions in `compatibility.json`. This mod is not yet in the [horizon-mods registry](https://playhorizon.dev/mods), and the native server half that would use the server export is still future work, so the shipped build uses the worker sidecar below.

Install the sidecar by hand until a registry entry and native server half are available:

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
| `V` | Post the latest screenshot from `screenshots/` to the configured `image_post_target` (always manual; see `post_screenshots` to auto-post every F2 shot) |
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
| `warrants <did\|handle>` | Read a player's active cosmetic ranks from pinned, signed issuer records (#192) |
| `verify <did\|handle>` | Cryptographically verify the logged-in account's handle via a fresh signed commit (#185) |
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
| `server_account_token_refresh_seconds` | `60` | Seconds between proactive server-account token checks; a healthy token stays an in-memory check (M50) |
| `labeler_enabled` | `false` | Publish the declaration on login and startup resume, request `identity:*` for the PLC identity update, and arm the game join/logout/sweep label triggers (M59–M62) |
| `persist_session` | `true` | Keep the refresh token in `session.json` across restarts |
| `post_death_messages` | `false` | Post a text post when you die |
| `post_achievement_messages` | `false` | Post a text post with 🏆 when you earn an advancement (#181) |
| `post_screenshots` | `false` | Automatically post each successful F2 screenshot to `image_post_target` (#182). The `V` keybind always works and is unaffected |
| `image_post_target` | `bluesky` | Network for image posts: `bluesky` (`app.bsky.feed.post` embed), `spark` (`so.sprk.feed.post`) or `grain` (`social.grain.photo` plus a gallery). All three write to the configured PDS with the logged-in account (#183) |
| `post_chat_messages` | `false` | Mirror every chat line to your repo |
| `player_data_sync_enabled` | `false` | Capture a linked player's aspects on leave and restore the newest verified snapshot on join (M68). Explicit `/atmospheric player-data` commands always work |
| `chat_server_id` | empty | Optional override for the chat record's `server` field; empty uses the signing key id, matching atpcraft |
| `skin_path`, `cape_path` | empty | Default PNGs for `/atmospheric skin` |
| `skin_model` | `default` | Skin model: `default` or `slim` |
| `oauth_client_id` | empty | Hosted client-metadata URL when loopback registration is unsupported |
| `oauth_scope` | empty | Base OAuth scope; empty uses `atproto transition:generic`. A narrow base gains the snapshot/skin/chat/blob grants the implemented writes need (M54), the selected `image_post_target`'s record grants (#183), the `equipment.rpg.item` grant when RPG discovery is on, and Colibri grants when the bridge is on |
| `rpg_items_enabled` | false | Publish discovered RPG items as an `equipment.rpg.give`/`equipment.rpg.item` pair; needs the server account and a linked player. Changing it requires a fresh OAuth login so the scope includes the RPG grants |
| `rpg_items_max_per_scan` | 10 | Maximum new RPG records written per scan |
| `client_name` | `atmospheric-horizon` | Fallback `name` for the server profile when `server_name` is blank |
| `server_name`, `server_description`, `server_address`, `server_website`, `server_contact` | empty | Server-profile identity; blank omits the field, and `server_name` falls back to `client_name` |
| `server_capabilities` | empty | Optional override; empty derives `join-gate` when armed, `skin-sync` and `labeler` when enabled |
| `game_version` | `26.3` | `minecraftVersion` in the server profile |
| `trusted_server_keys` | `[]` | `{key_id, public_key}` endorsements accepted on reads |
| `warrant_issuers` | `[]` | Pinned cosmetic-rank issuers (#192): `{did, collection, public_key}` (Ed25519 SPKI base64). A signed warrant is shown only when its DID, collection and key all match an entry here; empty trusts nobody |
| `colibri_bridge_enabled` | `false` | Arm the bridge after server-account login |
| `colibri_channel_uri` | empty | Channel AT-URI or Colibri app link |
| `colibri_appview_url` | `https://api.colibri.social` | Colibri AppView base URL |
| `colibri_appview_did` | `did:web:api.colibri.social` | AppView service DID; `#colibri_appview` is appended for the audience |
| `colibri_server_events_enabled` | `true` | Relay server join/leave/death/advancement lines into the channel |
| `colibri_server_events_fallback_enabled` | `true` | Fall back to the connection event when a chat mod hides/rewrites a join or leave line |

The app password, `server_link_token` and `server_admin_token` are the only secrets in `config.json`; OAuth sessions never store one.

## Server-side identity service

The optional `atmospheric-server` process supplies the shared Minecraft UUID → ATProto DID map that a client cannot build on its own. It is a separate HTTP service, not part of the game process. Set `server_api_url` to its base URL and configure `server_link_token` with the service's `ATMOSPHERIC_SERVER_LINK_TOKEN`. A player then runs `/atmospheric link` in game, gets a one-time URL in chat, and signs in with Bluesky in the browser. The game server creates that link token over an authenticated request, so the browser never chooses or supplies the UUID. The URL's lifetime is `browser_link_ttl_seconds` (default 600 seconds, accepted range 60–3600). `/atmospheric link <code>` still accepts an operator-issued M35 pairing code. With `join_gate_enabled` on, the server-side login hook resolves each joining UUID through this map and reads the configured public list before the player is placed. An unlinked player, a DID not on the list, or a failed read is refused. An unlinked player refused by an armed gate gets a one-time browser URL on the disconnect screen when `server_link_token` is set, so they can link and reconnect; if link creation fails, the generic refusal still fails closed.

Browser linking needs a public base URL reachable by the authorization server and the player's browser. OAuth requests go from this service to the handle resolver, the account's PDS, its authorization server and the account's PDS again; it does not contact the AppView for linking. Those are the service's only outbound requests. The game worker also calls this service's `POST /v1/browser/pairings` from `/atmospheric link` and from the gate refusal, so `server_link_token` must match on both sides.

Browser skin upload reuses the OAuth client with a separate `/skin/callback` redirect. Set `ATMOSPHERIC_SERVER_SKIN_KEY` to the game worker's `signing-key.json` (or a copy) to enable it. `/atmospheric skin upload` asks `POST /v1/browser/skins` for a one-time link only after the player's UUID is already linked; the page accepts a PNG up to 1 MiB, signs the browser in, requires the same DID, uploads the blob and upserts the existing `at.atpcraft.player.skin` record with the worker's Ed25519 endorsement. The link is single-use and uses the browser-link lifetime. Tokens, verifiers, DPoP keys and uploaded bytes stay in memory. The cape and other servers' endorsements are preserved. In-game application is not implemented because the pinned Horizon API has no skin/texture setter ([#113](https://github.com/ewanc26/atmospheric-horizon/issues/113)).

The same listener can act as a labeler. Set `ATMOSPHERIC_SERVER_LABELER_DID` to the server account's `did:plc` (or `did:web`) to enable it. The service creates an owner-only K-256 signing key at `<data>/labeler/signing-key.json` and appends signed labels to `labeler/labels.jsonl`; `POST /v1/labeler/labels` signs and stores one label with the admin token, and `GET /xrpc/com.atproto.label.queryLabels` serves active labels publicly. `GET /xrpc/com.atproto.label.subscribeLabels` is the spec WebSocket: it backfills the durable log from `cursor` (or the full history when omitted/0) and then streams new labels; a cursor past the tail gets a `FutureCursor` error frame and a normal close. Both public XRPC reads answer permissive CORS for browser consumers. Labels carry the subject DID/AT-URI and value, not player names or game data. The labeler HTTP service itself writes no declaration. With `labeler_enabled` on, the worker publishes `app.bsky.labeler.service` as the server account through its OAuth session (M59). `GET /v1/labeler/identity` reports the service DID, its `did:key` signing key and its public endpoint; `/atmospheric labeler-identity request` emails a one-time PLC token and `/atmospheric labeler-identity submit <token>` publishes `#atproto_label` and `#atproto_labeler` in the account's DID document while preserving every other entry (M60). `status` reports what the document currently has. With `labeler_enabled` and `server_admin_token` set, the worker also triggers the activity labels from game events (M61/M62). A linked player's join forces the next tick's sweep, which captures their op level and vanilla statistics and asserts `player` (30-day expiry, refreshed at most once a day), `online`, `admin` (op level at least 1), the highest playtime tier (10/100/1000 hours), `dragon-slayer` (one Ender Dragon kill), `monster-hunter` (1000 mob kills) and `death-defier` (100 deaths). Labels the current state no longer justifies are negated, except `vip` and `donor`, which are manual grants and are never touched. A disconnect negates `online`; a once-a-minute sweep re-evaluates online players; and worker startup negates `online` labels left by an unclean stop. The capture stays in the game and worker; only the subject DID and label values reach the labeler. Unlinked players and failed lookups are skipped, and every automatic trigger stays quiet on error.

| Variable | Default | Purpose |
| --- | --- | --- |
| `ATMOSPHERIC_SERVER_ADDR` | `0.0.0.0:8787` | Listen address |
| `ATMOSPHERIC_SERVER_DATA_DIR` | `./atmospheric-server-data` | Folder containing `links.json`, the labeler data and (when #189 is enabled) `operator/audit-key.json` and `operator/receipts.jsonl` |
| `ATMOSPHERIC_SERVER_TOKEN` | unset | Static admin bearer token; unset disables static-token admin routes and pairing creation, but DID grants can still authorize admin routes when #189 is enabled |
| `ATMOSPHERIC_SERVER_LINK_TOKEN` | unset | Shared secret the game server uses to create browser links; unset disables browser linking |
| `ATMOSPHERIC_SERVER_PUBLIC_URL` | unset | Public HTTPS base URL used for OAuth client metadata and the callback |
| `ATMOSPHERIC_SERVER_LABELER_DID` | unset | Server account `did:plc`/`did:web` to sign labels as; unset disables label emission and querying |
| `ATMOSPHERIC_SERVER_SKIN_KEY` | unset | Path to the game worker's `signing-key.json` for browser skin publishing; unset disables uploads |
| `ATMOSPHERIC_SERVER_OPERATOR_DIDS` | unset | Experimental #189: comma-separated `did:plc`/`did:web` allowlist for DID-authenticated admin grants; empty or unset disables the feature and creates no audit key or log |
| `ATMOSPHERIC_SERVER_OPERATOR_GRANT_TTL_SECONDS` | `900` | Experimental #189: grant lifetime in seconds (1–3600); too-long lifetimes are rejected at startup |
| `ATMOSPHERIC_SERVER_OPERATOR_SCOPES` | all five scopes | Experimental #189: comma-separated subset of `links_read`, `links_write`, `pairing_create`, `label_emit`, `audit_read` |
| `ATMOSPHERIC_SERVER_OPERATOR_HANDLE_SERVICE` | `https://bsky.social` | Experimental #189: handle resolver override for the operator's Proof-of-Handle check |
| `ATMOSPHERIC_SERVER_OPERATOR_PLC_DIRECTORY` | `https://plc.directory` | Experimental #189: PLC directory override for the operator's Proof-of-Handle check |

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

With the gate on, an unlinked player is refused with a browser URL in the disconnect reason. They open it, sign in with Bluesky, then reconnect. The same flow is available to an already-connected player through `/atmospheric link`. If `ATMOSPHERIC_SERVER_LINK_TOKEN` or `ATMOSPHERIC_SERVER_PUBLIC_URL` is unset, browser linking is disabled and the gate stays fail-closed.

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
| `POST /v1/operator/challenge` | public, allowlisted DID only | Issue a one-time DID challenge for an experimental #189 grant |
| `POST /v1/operator/grant` | public, DID proof | Redeem the challenge and #185 PDS-custody proof for a short-lived scoped grant |
| `GET /v1/operator/audit-key` | public | Public SPKI key for verifying signed audit receipts (#189) |
| `GET /v1/operator/receipts` | static admin token or `audit_read` grant | Read signed receipts for grant-exercised operations (#189); no token is stored in them |
| `POST /v1/browser/pairings` | link token | Create a one-time browser link for an online player (`ttl_seconds` optional, 60–3600) |
| `POST /v1/browser/skins` | link token | Create a one-time skin-upload link for an already-linked player |
| `POST /v1/labeler/labels` | admin | Sign and append one label for a subject (`uri`, `val`, optional `neg`/`cid`/`exp`) |
| `GET /v1/labeler/identity` | public | Labeler DID, signing `did:key` and public endpoint for the PLC identity update (M60) |
| `GET /xrpc/com.atproto.label.queryLabels` | public | Query active signed labels (`uriPatterns` required; `sources`, `limit`, `cursor`) |
| `GET /xrpc/com.atproto.label.subscribeLabels` | public (WebSocket) | Backfill from `cursor` and stream signed labels; `FutureCursor` past the tail |
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

#### DID-authenticated operator grants (experimental)

These four routes are additions to this repo's `/v1` surface, not Horizon
`/api/v1` routes. Set `ATMOSPHERIC_SERVER_OPERATOR_DIDS` to enable them; with an
empty allowlist they are disabled and no audit key or log is created. A grant
carries a subset of `links_read` (`GET /v1/links`), `links_write`
(`PUT`/`DELETE /v1/links/{uuid}`), `pairing_create` (`POST /v1/pairings`),
`label_emit` (`POST /v1/labeler/labels`) and `audit_read`
(`GET /v1/operator/receipts`). A valid grant with the wrong scope gets 403 and writes
a `scope_denied` receipt; static-token operations do not write receipts.

```bash
# 1. Ask for a challenge (only allowlisted DIDs get one).
curl -s -X POST http://127.0.0.1:8787/v1/operator/challenge \
  -H 'content-type: application/json' \
  -d '{"did":"did:plc:..."}'
# {"did":"did:plc:...","nonce":"...","issued_at":...,"expires_at":...}

# 2. As that DID, write a throwaway at.atpcraft.verify.nonce record containing
#    that nonce through your own PDS session (the #185 Proof-of-Handle write).
#    This sidecar never makes that write and never holds your session.

# 3. Redeem it for a short-lived scoped bearer token.
curl -s -X POST http://127.0.0.1:8787/v1/operator/grant \
  -H 'content-type: application/json' \
  -d '{"did":"did:plc:...","nonce":"<nonce from step 1>"}'
# 201 {"token":"...","did":"did:plc:...","scopes":[...],"issued_at":...,"expires_at":...}

# 4. Use the grant on a permitted /v1 admin route.
curl -X PUT http://127.0.0.1:8787/v1/links/00000000-0000-4000-8000-000000000001 \
  -H "Authorization: Bearer $GRANT_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"player_name":"Alex","did":"did:plc:...","handle":"alex.example"}'

# 5. Verify the public receipt key, then read the signed receipts.
curl -s http://127.0.0.1:8787/v1/operator/audit-key
curl -s -H "Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN" \
  http://127.0.0.1:8787/v1/operator/receipts
```

Warnings: this DID layer is experimental-quality on top of the stable control
API; the static token remains fully valid and unchanged on every admin route.
Compromise of an allowlisted operator's DID/PDS key is effectively admin
takeover, so keep the grant lifetime short (default 900 s, maximum 3600 s): a
stolen grant is a bearer token until it expires. Keep `<data>/operator` private,
since its Ed25519 key signs the receipts. Challenges and grants are in memory
only, so a restart invalidates them.
Receipts are append-only JSONL at `<data>/operator/receipts.jsonl`, signed with
the owner-only Ed25519 key at `<data>/operator/audit-key.json`; they record DID,
scope, action, target, grant id, outcome, key id and signature, never the bearer
token. All DID authentication stays inside the sidecar process boundary and
does not weaken the loopback/local-socket, token-gated control-plane assumption.
No player-facing gate reads this state. This proves PDS repository custody,
not human identity.

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

In game, run `/atmospheric link ABCD-EFGH` after setting `server_api_url` to the service base. Codes are one-time, default to ten minutes and accept 60–3600 seconds. Reissuing a code for the same UUID invalidates the previous one. Links and pending codes are written through an owner-only temporary file and an atomic rename on Unix (the Docker target) to `links.json`; a failed write leaves the old state in memory and on disk. Run the service in Docker with `docker compose --env-file docker/.env -f docker/compose.yaml up -d --build` after copying and editing `docker/.env.example`; `docker/.env.example` shows the required admin token, link token and public URL, plus the optional labeler DID. Run the end-to-end test with `bash docker/server-test.sh`.

Browser flow: run `/atmospheric link` with no code. The worker posts `{player_uuid, player_name, ttl_seconds}` to `/v1/browser/pairings` with the link token, receives a URL, and tells only that player to open it. The same request runs from a gate refusal for an unlinked player, with the URL on the disconnect screen. The page resolves the handle, pushes a PAR request, redirects to Bluesky, and exchanges the callback with PKCE and DPoP. The callback binds the signed-in DID to the UUID the game supplied. The link's lifetime is `browser_link_ttl_seconds` (default ten minutes, at most one hour). OAuth tokens and verifiers stay in memory, single-use, expire after ten minutes, and a service restart invalidates them.

`/atmospheric skin upload` uses the same browser flow with its own `/skin` routes. The service checks the player is already linked, takes a PNG up to 1 MiB, requires the signed-in DID to match that link, uploads `com.atproto.repo.uploadBlob`, and upserts `at.atpcraft.player.skin` at rkey = signing key id with the configured key's endorsement. The cape and other servers' endorsements are preserved; tokens, upload bytes and verifiers stay in memory, and the upload link is consumed after a successful publish.

M35 supplies the mapping, M36 lets a player claim a link in game, M37 checks a linked player's Colibri profile when they join, M38 enforces the configured list at login for linked accounts, M39 completes the browser flow, M40 puts a fresh browser link on an unlinked player's gate refusal, and M42 sweeps online players so a list edit takes effect without a reconnect. Labeler signing, durable storage, the public query endpoint, the live stream, the server-account declaration and its PLC identity publication are M56–M60; M61/M62 add the game triggers and the op-level/statistic labels. M63 adds manual, link-gated aspect capture/restore and RPG discovery, and M64 adds browser skin upload; automatic cross-server sync, core-NBT restore and in-game skin application remain future work.

## Roadmap

M1–M21 shipped through v0.1.0 and v0.2.0, M23 (social-graph writes) through M42 (online whitelist sweep) in v0.3.1, M44 (generic record browser) through M66 (profile avatar and banner uploads) in v0.4.0, the automatic post and image-target work through v0.7.0, Counting Stones through v0.8.0, Warrants through v0.9.0, verified handle plaques through v0.10.0 and Operator's Warrant through v0.11.0; the release history is in [CHANGELOG.md](CHANGELOG.md). Counting Stones (#193) adds in-game polls with a signed tally root and private receipts; Warrants (#192) reads cosmetic ranks from signed, pinned issuer records; Proof-of-Handle (#185) verifies a handle from a fresh signed commit; Operator's Warrant (#189) adds experimental DID-authenticated operator grants to the sidecar. What remains is registry submission (no longer blocked upstream, per ADR-002), the native server half that uses the trusted server export, automatic cross-server sync, core player aspects, in-game skin application, and the next read/write surfaces.

### Porting atpcraft

atpcraft is a server-side Fabric mod, while Horizon native mods can hook the server side too (`native_tick`/`native_world_lifecycle` run there). Its lexicons are reimplemented here as data — no code is copied, only the wire formats are matched.

| atpcraft feature | atmospheric-horizon |
| --- | --- |
| Bluesky sign-in | **M8**: localhost OAuth login (`/atmospheric login <handle>` — PAR + PKCE + DPoP with a loopback redirect); app-password login remains the fallback via bare `/atmospheric login`. **M53** makes the requested base scope configurable (`oauth_scope`); **M54** appends the snapshot/skin/chat/blob grants to a narrow base and gives the server account its descriptor and stream grants |
| Profile pronouns and display names | **M24**: `/atmospheric profile <did\|handle>` reads the AppView's `app.bsky.actor.getProfile` and the direct `app.bsky.actor.profile` record at rkey `self`. The direct record's pronouns win when that read succeeds (even when the field is absent); a failed record read falls back to the AppView, matching atpcraft's precedence. Both reads are unauthenticated. **M45** composes the same display name and pronouns into a linked joining player's in-game name on a server; **M47** first reduces the display name to atpcraft's valid 16-character Minecraft name (handle username fallback) and keeps the suffix plain text, so atpcraft's gray chat-only suffix is not reproduced |
| Chat messages as records (`at.atpcraft.player.chat`) | **M5**: the chat hook mirrors each line, and the worker publishes TID-keyed records when `post_chat_messages` is on; `/atmospheric chat <text>` always publishes one. **M16** (read side): `/atmospheric chat-of <did\|handle>` lists the collection on the subject's own PDS, unauthenticated, and reports the count plus the newest messages — chat records are unsigned by design. **M48** writes the server's Ed25519 key id into the `server` field by default (an explicit `chat_server_id` overrides it), matching atpcraft's chat writer |
| Player state snapshots (`at.atpcraft.player.snapshot`, `defs`) | **M6**: the keybind or `/atmospheric snapshot` publishes a self-endorsed record (the worker generates and persists an Ed25519 key; rkey = key id, monotonic version). **M13** (read side): `/atmospheric player <did\|handle>` pages `com.atproto.repo.listRecords`, takes the highest `version`, and verifies the endorsement against your own key or `trusted_server_keys` |
| Reading other players' snapshots (presence) | **M13**: as above — the client-side half of atpcraft's player-data sync |
| Skins and capes as blobs on the PDS | **M9**: skin/cape PNGs are uploaded as blobs and published as an Ed25519-endorsed `at.atpcraft.player.skin` record (rkey = key id, `putRecord` upsert) via `/atmospheric skin [path] [default\|slim]`. **M14** (read side): `/atmospheric skin-of <did\|handle>` verifies the endorsement against your own key or `trusted_server_keys`, then downloads the skin/cape blobs into the mod's `skins/` folder — only when the record verifies. **M64** adds the browser write for an already-linked player, signed with the worker's key and preserving the cape and foreign endorsements; in-game application is still blocked upstream |
| Labeler publishing player activity | **M12** reads labels on a subject via the AppView's `labels` fields (`/atmospheric labels <subject>`), and queries `com.atproto.label.queryLabels` at a labeler's `#atproto_labeler` DID-document endpoint (`/atmospheric labeler <did> [subject]`). **M56** adds canonical CBOR and K-256 signing; **M57** adds owner-only key persistence, the durable label log, the admin `POST /v1/labeler/labels` write and the public `queryLabels` endpoint; **M58** adds the spec `subscribeLabels` WebSocket with cursor backfill, live fan-out and `FutureCursor`, plus permissive CORS. **M59** adds the `labeler_enabled` setting and publishes the `app.bsky.labeler.service` declaration as the server account; **M60** publishes `#atproto_label` and `#atproto_labeler` in the account's DID document; **M61** adds the game join/logout/sweep triggers for `player` and `online`, with a daily badge refresh and startup recovery; **M62** captures op level and vanilla statistics and adds `admin`, the three playtime tiers and the three achievement labels, negating labels the current state no longer justifies while leaving `vip` and `donor` alone |
| Server profile record (`at.atpcraft.server.profile`) | **M10**: published at rkey `self` via `putRecord` with `/atmospheric server` (identity fields from `config.json`, `signingKey` block = the mod's Ed25519 key, `createdAt` preserved across re-publishes). **M19** (read side): `/atmospheric server-of <did\|handle>` lists the collection and reports the server name, MC version, capabilities, and whether its published signing key is trusted, unknown or mismatched. **M49** falls back to `client_name` when `server_name` is blank and derives capabilities (`join-gate`, `skin-sync`, and `labeler` when enabled) unless `server_capabilities` overrides them. **M51** re-publishes the descriptor after a server-account login and on startup resume, and **M59** advertises `labeler` when enabled |
| Join gate reading a public Bluesky list | **M11** (read side): `/atmospheric list <at-uri \| bsky.app URL>` fetches member DIDs via paginated `app.bsky.graph.getList`. **M15** (the decision): `/atmospheric gate <list> <did\|handle>` reports whether the DID is on the list, fail-closed. **M38** enforces it at the server login phase for linked UUIDs; **M39** adds browser self-service linking, **M40** puts that link on the refusal screen, and **M42** sweeps online players every `join_gate_sweep_seconds` and disconnects a linked player whose DID has left the list. **M52** caches members for `list_cache_seconds`, refreshes on a stale cache or a fresh miss, and reuses the cache across explicit reads, gate checks and sweeps |
| Colibri chat bridge | **M26–M34**: `/atmospheric colibri start\|stop\|reconnect\|status\|send <text>` parses a channel AT-URI or app link, declares the server account's community membership, mints and rotates the service-auth token, holds the `social.colibri.sync.subscribeEvents` WebSocket, shows channel messages in game chat, publishes game chat as the player account, and relays server join/leave/death/advancement broadcasts as the server account. **M29** adds the 3-second connection-event fallback for hidden/rewritten join/leave lines and a pending-approval HUD notice when the community requires it. **M30** checks the local player's `social.colibri.actor.profile` when the bridge starts and shows atpcraft's setup prompt if it is absent; **M37** checks other linked players when they join and sends the prompt to them. **M31** adds `/atmospheric colibri reconnect` and atpcraft's 20-per-minute player send limit. **M32** writes the local player's own membership when the bridge starts or they log in, and shows atpcraft's pending line for a new approval-gated membership. **M33** applies atpcraft's control/whitespace cleanup, 64-character handle cap and 512-character text cap to incoming messages. **M34** carries the community URI and shows it in the bridge status |
| RPG item discovery (`equipment.rpg.*`) | **M63**: `/atmospheric rpg discover [player]` scans the linked player's vanilla picked-up/crafted/used/broken item statistics and renamed inventory/ender-chest/equipment stacks, excluding wearables. With `rpg_items_enabled`, the worker writes an `equipment.rpg.give` attestation from the server account and an `equipment.rpg.item` record in the player's repo, using atpcraft's deterministic keys and `rpg_items_max_per_scan` cap. The player's UUID must map to the worker's own DID |
| Player-data aspect capture/restore | **M63**: `/atmospheric player-data capture [player]` uploads gzip-NBT `inventory`/`ender_chest`/`equipment` wrappers and raw `advancements`/`stats` JSON as snapshot blobs; `/atmospheric player-data restore [player]` verifies the logged-in account's latest snapshot, downloads the supported blobs and applies them. The UUID must be linked to the logged-in DID. Full atpcraft parity is narrower here: `core`, `ops`, `scoreboard`, `team`, `effects`, `respawn`, `recipes` and `chat` aspects are not restored; **M68** adds the automatic hooks |
| Automatic player-data sync | **M68**: with `player_data_sync_enabled` (default off), `native_player_logging_out` captures the five M63 aspects while the player is still in their level, and `player_logged_in` requests the newest verified snapshot for the tick to apply. Hook-originated commands are silently ignored while the setting is off; explicit commands always run. Restore accepts an endorsement from the local key or `trusted_server_keys`, so a peer server's signed snapshot is portable. A full `core` NBT restore is still out of scope — Horizon has no complete round-trip setter for the player compound |
| Server account | **M25**: `/atmospheric server-account login <handle>` runs a separate localhost OAuth flow and keeps the result in `server-session.json`; `status`, `logout` and `publish` manage it. The server profile can then be published as that account (`/atmospheric server-account publish`), as atpcraft's server does. **M50** keeps its OAuth access token fresh on a configurable cadence, so an idle bridge or descriptor publish does not discover an expired token. **M51** re-publishes the descriptor automatically after a login and when a persisted session resumes at startup, so the public record follows the configured identity without a manual command. **M59** uses the same session to upsert the labeler declaration. It is the identity foundation for the remaining server-side work |
| Server-wide player identity map | **M35–M40**: optional `atmospheric-server` process with public UUID reads, admin-token writes, one-time pairing claims, browser OAuth linking and atomic persistence. `/atmospheric link <code>` claims a code; `/atmospheric link` starts a one-time browser flow bound to the online player's UUID, and **M40** issues the same link from an unlinked gate refusal. A join uses the public map for the Colibri profile check and, with the gate armed, for the login-phase list decision. Player-data consumers are not implemented yet |
| Labeler issuance | **M56–M62** cover signed labels, owner-only key persistence, the durable log, the public query endpoint, the live subscription WebSocket, the server-account `app.bsky.labeler.service` declaration, its PLC identity publication and the game triggers for the full automatic vocabulary — `player`, `online`, `admin`, three playtime tiers and three achievements. `vip` and `donor` remain manual |
| Web server for login pages / lexicons | **M39** hosts the browser link pages and OAuth client metadata in `atmospheric-server`; **M46** serves the five `at.atpcraft.*` lexicon documents and an index at `/lexicons`; **M57/M58** serve the label query, admin emission and live subscription routes, **M59** writes the declaration from the worker, **M60** publishes the labeler identity through the server account's PLC operation, **M61** triggers `player`/`online` emission from game events, **M62** adds op-level and statistic labels, and **M64** hosts the one-time skin upload pages, callback and signed publish |

## Network use

This mod talks to the network **by design**: the AT Protocol services needed to reach and authenticate with your account's PDS, plus the public AppView, labelers, and the optional identity service you configure for link claims, browser link creation, join-time profile checks, labeler triggers and (experimentally) DID-authenticated operator grants — nothing else. Image posts always use your configured PDS; `image_post_target` changes only which record collection is written there (#183). All game-side network I/O happens in the spawned local `atmospheric-worker` process, never in the game; the game and the worker exchange plain JSON commands and events over a local pipe only. The separate `atmospheric-server` listener performs browser OAuth, serves the public lexicon documents and, when configured, stores and serves signed labels as described below. With #189 enabled it also serves four routes on **this repo's `/v1` surface, not Horizon's `/api/v1`**: `POST /v1/operator/challenge`, `POST /v1/operator/grant`, public `GET /v1/operator/audit-key`, and `GET /v1/operator/receipts` (static admin token or a grant with `audit_read`). Grants are scoped to `links_read`, `links_write`, `pairing_create`, `label_emit` and `audit_read`; a valid grant with the wrong scope receives 403 and writes a `scope_denied` receipt, while static-token operations write no receipt. The operator writes the throwaway `at.atpcraft.verify.nonce` record through their own PDS session; this sidecar performs only unauthenticated verification reads plus the existing handle and DID resolution (the configured handle service, PLC directory and the DID document's PDS), so the operator DID, nonce and public commit data go to those services. Receipts are stored at `<data>/operator/receipts.jsonl` with an owner-only Ed25519 key at `<data>/operator/audit-key.json`.

### Identity and login

- Handle resolution uses `com.atproto.identity.resolveHandle` on `https://bsky.social`, and DID documents come from `https://plc.directory` (did:plc) or the domain's `/.well-known/did.json` (did:web). The DID document supplies the PDS's `#atproto_pds` service endpoint and a labeler's `#atproto_labeler` endpoint. The M60 identity update also reads the account's current operation from `https://plc.directory/{did}/data` and submits the merged operation through the account's PDS.
- OAuth (M8) discovers the PDS's `/.well-known/oauth-protected-resource` and the authorization server's `/.well-known/oauth-authorization-server`, then sends a pushed authorization request (`POST <authorization server>/oauth/par`) with PKCE and a DPoP key thumbprint. The authorize page opens in your system browser (the URL is always shown first; if no browser can launch, the worker tells you to open it manually), and the redirect is received on `http://127.0.0.1:<port>/` (loopback only). The code is exchanged at the authorization server's token endpoint (`POST <authorization server>/oauth/token`), which is also used for refreshes.
- Every request to the authorization server, and every PDS request while OAuth-logged-in, carries `Authorization: DPoP <access token>` plus a `DPoP` proof JWT (ES256, signed by a per-install P-256 key). The client registers as the loopback client `http://localhost?scope=…`. The scope is `atproto transition:generic` unless `oauth_scope` sets a different base. A narrow base gains the snapshot, skin, chat and `blob:*/*` grants the implemented writes need, plus only the selected `image_post_target`'s record grants (#183); when the Colibri bridge is enabled the player scope adds the `social.colibri.message`/`membership` repo grants, while the server-account scope also adds `social.colibri.sync.subscribeEvents?aud=…`, which the PDS requires before it will mint the AppView token. When `labeler_enabled` is on, the server-account scope also adds the labeler-service declaration and `identity:*` grants (the PDS requires `identity:*` to sign a PLC operation). A custom scope must also be declared by a hosted client-metadata document, including the server-profile, stream, labeler-service and identity grants the server account requests. If your PDS's authorization server does not support the loopback form, set `oauth_client_id` to a hosted copy of `oauth-client-metadata.json`. This repo serves one at `https://ewanc26.github.io/atmospheric-horizon/oauth-client-metadata.json` ([#21](https://github.com/ewanc26/atmospheric-horizon/issues/21)).
- `/atmospheric server-account login <handle>` runs the same OAuth flow for a second account and persists it separately; a successful login re-publishes the server profile as that account, and a persisted session does the same when it resumes at startup. `/atmospheric server-account publish` remains available for a manual refresh. Enable the Colibri bridge before logging in this account so its scope contains the stream grant; a session created earlier must be logged out and authorised again. The worker also refreshes that account's access token proactively every `server_account_token_refresh_seconds` (default 60), using the same token endpoint; its expiry check skips the network until the token is near expiry. The automatic descriptor write uses the same `getRecord`/`putRecord` pair as the manual command and preserves `createdAt`. With `labeler_enabled` on, the same login and resume paths also upsert `app.bsky.labeler.service` at rkey `self`, preserving its `createdAt`; enable the flag before logging in so the server-account scope includes the declaration and identity grants, or log out and authorise again. `/atmospheric labeler-identity request` asks the PDS to email a one-time PLC token, and `submit <token>` reads the labeler's public identity from `<server_api_url>/v1/labeler/identity`, reads the account's current operation from `https://plc.directory/<did>/data`, and asks the PDS to sign and submit it; `status` reports the published entries.
- App-password login uses `com.atproto.server.createSession`, checks the session with `com.atproto.server.getSession`, and resumes restarts with `com.atproto.server.refreshSession` (or the OAuth token endpoint's `refresh_token` grant). `/atmospheric logout` drops the session.

### Public reads

All of these are unauthenticated, and the query carries only the subject's DID or the requested record. No credentials or player state are sent.

- **Lists and the join gate (M11/M15/M38/M42/M52):** `app.bsky.graph.getList` (paged, limit 100) against `appview_url` for `/atmospheric list`, `/atmospheric gate`, the login gate, and the periodic online sweep. Members are cached for `list_cache_seconds` (default 300): a fresh hit sends no request, a stale cache refreshes once, and a fresh gate miss refreshes once before denying. A failed read is a deny-with-reason, never a silent allow; a failed sweep read disconnects nobody. The login gate and the sweep also read `<server_api_url>/v1/links/{uuid}` to resolve a player before the list read.
- **Labels (M12):** `app.bsky.actor.getProfile`, `app.bsky.feed.getPosts` and `app.bsky.graph.getList` (first page, limit 1 — the view carries the labels) against the same AppView for `/atmospheric labels`; and `com.atproto.label.queryLabels` (paged, limit 100) at the labeler's `#atproto_labeler` endpoint for `/atmospheric labeler`. The M61/M62 triggers also read `<server_api_url>/v1/links/{uuid}` and query `<server_api_url>/xrpc/com.atproto.label.queryLabels` for the linked DID's active labels before emitting through the admin route.
- **Profiles (M24/M45):** `app.bsky.actor.getProfile` (query: the subject's DID) against `appview_url`, plus `com.atproto.repo.getRecord` (query: the subject's DID, collection `app.bsky.actor.profile`, rkey `self`) against the subject's own PDS, for `/atmospheric profile`. The direct record's `pronouns` is preferred when that read succeeds; a failed direct read falls back to the AppView value. Nothing is written or stored. On a server, the same two reads also run for a joining player's linked DID (M45/M47); an unlinked player is not read, and the result is used only as their in-game name.
- **Presence (M13):** `com.atproto.repo.listRecords` for `at.atpcraft.player.snapshot` (paged, limit 100) against the subject's own PDS for `/atmospheric player`.
- **Skins (M14):** `com.atproto.repo.listRecords` for `at.atpcraft.player.skin`, then — only when the endorsement verifies — `com.atproto.sync.getBlob` for the skin/cape blob CIDs (query: the subject's DID and the CID) to download the PNGs.
- **Chat history (M16):** `com.atproto.repo.listRecords` for `at.atpcraft.player.chat` against the subject's own PDS for `/atmospheric chat-of`. Chat records are unsigned by design, so nothing is verified.
- **Generic records (M44):** `com.atproto.repo.listRecords` (paged, limit 100) for the requested collection against the subject's own PDS for `/atmospheric records`. The query carries only the subject's DID and the collection name; nothing about the requester goes out, and nothing is written or stored.
- **Server profiles (M19):** `com.atproto.repo.listRecords` for `at.atpcraft.server.profile` against the subject's own PDS for `/atmospheric server-of`.
- **Server cards (M69):** `com.atproto.repo.listRecords` for `click.croft.atmospheric.server.card` against the subject's own PDS, for `/atmospheric card-of <did|handle>`: the typed manifest and a liveness verdict graded from the record's `createdAt` recency against the reader's clock (`Fresh` within an hour, `Stale` after, `Unknown` when the timestamp does not parse). A repo with no valid card reports absence, not an error; nothing is written or stored.
- **Counting Stones tallies (#193):** `com.atproto.repo.listRecords` for `click.croft.atmospheric.poll.result` against the subject's own PDS, for `/atmospheric poll result <did|handle>`: the newest structurally valid record is parsed and its Ed25519 signature verified with the public key it carries. The query carries only the subject; nothing about the requester is sent, and nothing is written or stored. Receipts are never queried — they stay in the worker and are shown only to the voter who requested one.
- **Warrants (#192):** `com.atproto.repo.listRecords` for each collection pinned in `warrant_issuers` against the subject's own PDS, for `/atmospheric warrants <did|handle>`. The query carries only the subject and the pinned collection names; nothing about the requester is sent and nothing is written or stored. The issuers, DIDs and public keys come from the worker's own `config.json`, never from the game, so a caller cannot self-attest a rank. An empty allowlist resolves the subject but sends no `listRecords` call.
- **Handle verification (#185):** `/atmospheric verify <did|handle>` first resolves the subject and the `#atproto_pds` endpoint through the existing public resolver, then writes one `at.atpcraft.verify.nonce` record (fields: nonce and `createdAt` only) through your own PDS session. It then reads `com.atproto.sync.getLatestCommit` and `com.atproto.sync.getBlocks` from that PDS, and the DID document and `#atproto` key from `plc.directory`. No credential, profile field or gameplay data is sent; the throwaway record is the only write and no other endpoint is contacted. The check proves PDS repository custody, not human identity.
- **Social graphs (M20):** `app.bsky.graph.getFollows`, `getFollowers`, `getBlocks` and `getMutes` (paged, limit 100) against `appview_url` for `/atmospheric follows|followers|blocks|mutes`. The member list comes back to you; nothing about you goes out.
- **Social-graph write lookups (M23):** undoing a follow or block uses `com.atproto.repo.listRecords` for `app.bsky.graph.follow`/`block` against your own PDS (public, unauthenticated) to find records whose `subject` is the target DID. The create/delete/mute calls themselves are in the writes section below.
- **Interaction targets (M21):** `com.atproto.repo.getRecord` from the target author's PDS (query: their DID, collection `app.bsky.feed.post`, and record key) to obtain the post's CID and, for replies, its thread root.
- **Colibri stream and membership (M26/M27/M32):** the worker lists the server account's `social.colibri.membership` records and creates one for the bridged community when absent; when the bridge starts or the local player logs in, it also repairs the player's own membership the same way. It then opens `wss://<appview>/xrpc/social.colibri.sync.subscribeEvents` with `Sec-WebSocket-Protocol: colibri.auth.bearer, <service-token>`. It sends a heartbeat every 30 seconds, reconnects with backoff, and rotates the token at 45 minutes: the socket closes, the worker mints a fresh token from `com.atproto.server.getServiceAuth` on the server account's PDS (audience `<colibri_appview_did>#colibri_appview`), and reconnects. `/atmospheric colibri reconnect` does the same mint-and-reconnect on demand without repairing membership or re-checking the profile. Only message upserts are used; other event types are ignored. The stream does not send player data beyond the account identity already carried by the token.
- **Colibri server notices (M28):** the server-side `broadcast_message` hook passes join, leave, death and advancement translation keys to the worker. Ordinary chat and unknown lines are ignored. The worker writes the localized line as `social.colibri.message` under the server account, filtered only by `colibri_server_events_enabled`; no player data is added.
- **Colibri presence fallback (M29):** server-side `player_logged_in` and `player_logged_out` hooks schedule a fallback notice. The worker waits 3 seconds and writes it only when no join/leave line, or a blank/hidden line, arrived within 5 seconds of the event. `colibri_server_events_fallback_enabled` (default true) controls the fallback. After a new membership is created, the worker reads the community record's `requiresApprovalToJoin` with an unauthenticated `com.atproto.repo.getRecord`; if true, a HUD status line says a moderator must approve the join. No account mapping or extra player data is added.
- **Colibri community approval read (M29):** the channel's community DID is resolved through its DID document to its PDS, then `com.atproto.repo.getRecord` requests `social.colibri.community` at rkey `self`. This is the only new read added by M29; it runs only after a new membership is created.
- **Colibri profile prompt (M30/M37):** when the bridge starts, the worker reads the local player's own `social.colibri.actor.profile` at rkey `self` with `com.atproto.repo.getRecord`, authenticated as the player. A missing record shows atpcraft's two-line setup prompt with `https://colibri.social`; a failed read is quiet. For a joining player, the server-side hook passes their UUID and name; the worker resolves `GET <server_api_url>/v1/links/{uuid}` (public), then reads the linked DID's actor profile at rkey `self` unauthenticated on its own PDS. A missing record sends the same two-line prompt to that player with `native_tell`; unlinked players and failed reads are quiet.
- **Join gate (M38/M40):** before a player is placed, the server-side `player_login` hook waits for the worker to resolve `GET <server_api_url>/v1/links/{uuid}` and to read the configured public list. A linked DID on the list is admitted; an unlinked UUID, a DID not on the list, an unset list, a missing service URL, an error, or a timeout is refused. For an unlinked UUID with `server_link_token` set, the same gate read also sends `POST <server_api_url>/v1/browser/pairings` with `{player_uuid, player_name, ttl_seconds}` and the link token, and puts the returned one-time URL on the disconnect screen. Link-creation failure falls back to the generic refusal. The hook waits at most 3 seconds for the worker's startup state and 6 seconds for the decision; the worker bounds the identity, list and link reads at 5 seconds.

### Writes to your PDS

- Notification polling uses `app.bsky.notification.listNotifications` with a `seenAt` timestamp, on a timer and on `/atmospheric notifications`.
- Text posts and the post-location keybind publish `app.bsky.feed.post` with `com.atproto.repo.createRecord`; death messages and achievement announcements use the same record, only when `post_death_messages`/`post_achievement_messages` is enabled. Image posts upload bytes once with `com.atproto.repo.uploadBlob`, then write the record shape chosen by `image_post_target` (#183): Bluesky's `app.bsky.feed.post` with an `app.bsky.embed.images` embed, Spark's `so.sprk.feed.post` with `so.sprk.media.images`, or Grain's `social.grain.photo` plus a `social.grain.gallery` and a linking `social.grain.gallery.item` (the gallery URI is reported as the post). All three go to the configured PDS as the logged-in account; the caption or gallery title is capped locally (Spark: 3000 bytes/300 scalars; Grain: 100 bytes), images are capped at 2 MB for Bluesky/Spark and 1 MB for Grain, and Grain accepts only PNGs with readable dimensions because its photo lexicon requires an aspect ratio. Each F2 screenshot is offered as an automatic image post too, but is only uploaded and published when `post_screenshots` is enabled (#182); the manual `V` keybind posts the latest shot regardless of that flag. No new endpoint, AppView call or credential is involved.
- Chat lines use `com.atproto.repo.createRecord` in the `at.atpcraft.player.chat` collection, TID-keyed — only when `post_chat_messages` is enabled, or one at a time via `/atmospheric chat <text>`. While the Colibri bridge is active, the same chat line is also written to `social.colibri.message` in the player's repo, keyed by a fresh TID; `/atmospheric colibri send <text>` writes one without posting the game chat record. Player-authored Colibri writes are capped at 20 per rolling 60 seconds; the 21st is dropped, server notices are exempt, and the explicit command reports the limit.
- Colibri membership (M27/M32) lists `social.colibri.membership` in the server account's and local player's own repos and creates one `{community, createdAt}` record when the bridged community is absent. A new player membership in an approval-gated community produces atpcraft's pending line; existing memberships and failed writes stay quiet. It uses `createRecord`, not `putRecord`, because the Colibri OAuth grant contains create/delete but not update.
- Snapshots use `com.atproto.repo.createRecord` in `at.atpcraft.player.snapshot`, carrying your position/dimension and an Ed25519 endorsement signature.
- Skins use `com.atproto.repo.uploadBlob` for each PNG, `com.atproto.repo.getRecord` to keep other servers' endorsements, then `com.atproto.repo.putRecord` in `at.atpcraft.player.skin` at the endorsing key id.
- Profile edits (M65/M66) fetch `app.bsky.actor.profile` at rkey `self`, merge one field and `putRecord` it back with PDS validation. Text fields (`displayName`, `description`, `pronouns`) are written directly and an empty value removes the field. For `avatar` and `banner`, a local PNG/JPEG up to 1,000,000 bytes is uploaded with `com.atproto.repo.uploadBlob` and the returned blob reference is merged. The other image, text fields, links, pinned post and `createdAt` are preserved; a missing `createdAt` is added. Only the logged-in account's own profile is written.
- The server profile uses `com.atproto.repo.getRecord` to preserve `createdAt`, then `com.atproto.repo.putRecord` in `at.atpcraft.server.profile` at rkey `self`. `/atmospheric server` writes it as the player account; `/atmospheric server-account publish` writes it as the separate server account (M25).
- Counting Stones (#193) closes a poll with one `com.atproto.repo.createRecord` write in `click.croft.atmospheric.poll.result` at the caller's record key, as the logged-in account. The record contains the question, choices, close and publish times, the signed Merkle root, the participant DID list and the aggregate tally — never a choice next to a DID. Opening a poll and voting are local worker operations: the worker resolves each voter's linked DID through the existing public `GET <server_api_url>/v1/links/{uuid}` read, keeps the salted choice and salt private, and emits a vote event without the choice. `/atmospheric poll receipt <id>` returns that voter's receipt from worker memory only; no receipt, salt, choice or nullifier is ever written to the PDS or sent to the game for another player. Signing uses the worker's existing persisted Ed25519 key through `signing-key.json`; the game process never receives it. No new endpoint or credential is involved.
- The server card (M69) uses `com.atproto.repo.putRecord` in `click.croft.atmospheric.server.card` at rkey `self`, written by the separate server account only. `/atmospheric server-card publish` writes it on demand; a server also republishes every five minutes (only when a player-slot ceiling is reported, so clients stay silent). Identity fields and the pinned Horizon revision come from the settings file and `compatibility.json`; the population counts come from the live server; the record is validated (byte caps, lower-hex checksums, https and at:// scheme rules) before it is sent, and every publish stamps a fresh `createdAt`, so the record's recency is the card's liveness. Packs declare none yet — Horizon's signed-registry `(h)pack` checksum lookup is a later slice. No new endpoint or credential is involved — the write rides the server account's existing PDS session.
- The M59 labeler declaration uses `com.atproto.repo.getRecord` then `com.atproto.repo.putRecord` for `app.bsky.labeler.service` at rkey `self`, as the server account, after login and startup resume. It declares the 11-value player-label vocabulary and its client metadata. The M60 `/atmospheric labeler-identity` path is separate: it reads `GET <server_api_url>/v1/labeler/identity` and the public PLC operation, then uses `com.atproto.identity.signPlcOperation` and `submitPlcOperation` on the server account; neither path emits a label. The M61/M62 game triggers are the emission path: they read and write labels through the identity service's public query and admin emission routes, not the PDS.
- Interactions use `com.atproto.repo.createRecord`: `app.bsky.feed.like` or `app.bsky.feed.repost` with a `subject` strong ref `{uri, cid}`, or an `app.bsky.feed.post` with `text`, `createdAt` and `reply: {root, parent}` strong refs.
- Social-graph writes (M23) resolve the subject to a DID, then:
  - `/atmospheric follow` and `/atmospheric block` write `app.bsky.graph.follow` / `app.bsky.graph.block` with `subject` and `createdAt`, keyed by a fresh TID.
  - `/atmospheric unfollow` and `/atmospheric unblock` find matching records in your own repo (the `listRecords` read above) and delete each with `com.atproto.repo.deleteRecord`.
  - `/atmospheric mute` and `/atmospheric unmute` call `app.bsky.graph.muteActor` / `unmuteActor` on the AppView. Mutes are private and create no record. No new data is stored locally.

### Stored on disk

Everything lives in the worker's own data folder — never in a world save. In sidecar mode that is `<game exe dir>/atmospheric-data/`; under Horizon's native storage API it is `horizon-mods/.horizon-mod-data/atmospheric/`.

- `config.json` holds the settings above, including the PDS URL, identifier, app password, poll interval, server-account token refresh interval, labeler declaration toggle, post flags and image-post target, chat server id, skin/cape paths and model, server-profile fields, game version, AppView URL, identity-service URL, its admin token for the M61/M62 labeler triggers, and trusted endorsement keys.
- `signing-key.json` holds the Ed25519 seed for snapshot and skin endorsements, generated once.
- `session.json` holds the player's refresh token used to resume across restarts. For OAuth sessions it also holds the DPoP private key (JWK) and token endpoint; app passwords are never stored in it. Delete it or use `/atmospheric logout` to revoke a session.
- `server-session.json` holds the same OAuth material for the separate server account (M25), written owner-only. It is independent of the player session and is deleted by `/atmospheric server-account logout`.
- `cursor.json` (the notification seen-cursor, so restarts do not re-show old notifications) and `snapshot-state.json` (the snapshot version counter, so records never roll back) are not secret. The Colibri bridge stores no additional credential: its service token lives in memory and is minted again on start.
- `skins/` holds skin/cape PNGs downloaded by `/atmospheric skin-of`, content-addressed by blob CID. They are written only for records whose endorsement verified.

The files that hold secrets are written with owner-only permissions (0600 on unix; the OS file ACLs apply on Windows and macOS). The image you post is read from the game's `screenshots/` folder only when you press the post-screenshot keybind, or when an F2 screenshot is posted automatically with `post_screenshots` enabled; otherwise the latest path stays in memory. Skin/cape PNGs are read from the path you pass (or `skin_path`/`cape_path`) only at that moment; nothing else is read. The login session is held in memory for the running game; the refresh token is persisted so restarts resume automatically unless `persist_session` is off.

### Server-side identity service (M35–M40)

The optional `atmospheric-server` process listens only on `ATMOSPHERIC_SERVER_ADDR`. `GET /healthz`, `GET /v1/links/{uuid}` and the OAuth client metadata are public; admin operations on this surface accept `Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN` unchanged, and additionally (experimentally) a scoped #189 DID grant carrying the route's scope; browser-link creation requires `Authorization: Bearer $ATMOSPHERIC_SERVER_LINK_TOKEN`. Claiming a pairing code and opening a browser link are public because the one-time token is the credential. The service stores `links.json` in its own data folder: canonical player UUIDs, player names, DIDs, optional handles, first-link timestamps and pending codes with their expiry. Browser link tokens, PKCE verifiers and DPoP keys stay in memory and are not persisted. It has no telemetry. It also serves the public `at.atpcraft.*` lexicon documents; those responses read no player data. M36's `/atmospheric link <code>` posts the code, the player's DID and optional handle to `<server_api_url>/v1/pairings/claim`; it does not send a session token or game data. M39's `/atmospheric link` posts `{player_uuid, player_name}` with the server-only link token. The browser service resolves the handle and DID, discovers the PDS and authorization server, pushes PAR, and exchanges the callback; no game data beyond the UUID/name binding is sent. M64's `/atmospheric skin upload` posts `{player_uuid, player_name}` to `/v1/browser/skins` with the link token and gets a one-time `/skin/{token}` URL; that page sends the PNG (at most 1 MiB) to the service, which uploads it to the linked account's PDS as a blob and writes the signed `at.atpcraft.player.skin` record after a DPoP browser login as the linked DID. M37 reads `GET /v1/links/{uuid}` when a player joins, to check the linked account's Colibri profile, and M38 reads the same public route at login to enforce the configured list. M40's gate refusal additionally creates a browser link through `POST /v1/browser/pairings`, carrying the UUID, player name and requested lifetime. With `ATMOSPHERIC_SERVER_LABELER_DID` set, M57 adds a local K-256 key at `labeler/signing-key.json` and an append-only `labeler/labels.jsonl`; neither is sent anywhere. The admin emission route stores only the submitted subject DID/AT-URI, value and timestamps, and the public `queryLabels` route returns those stored labels. M58 adds the live `subscribeLabels` WebSocket over the same log and permissive CORS on the public XRPC reads; the stream sends only those stored labels. No outbound request is made for labelling.

#189's four operator routes are also on this repo's `/v1` surface, not Horizon's `/api/v1`: `POST /v1/operator/challenge`, `POST /v1/operator/grant`, public `GET /v1/operator/audit-key` and `GET /v1/operator/receipts` (static token or an `audit_read` grant). An allowlisted DID receives a nonce, writes a throwaway `at.atpcraft.verify.nonce` record through its own PDS session as in #185, and redeems the challenge for a short-lived scoped token. The sidecar's own network use for this path is unauthenticated verification reads plus the existing handle/DID resolution through the configured handle service, PLC directory and DID document; it sends no credentials and never writes to the operator's repo. Challenges and grants are in memory only. Every grant-exercised admin operation appends a signed receipt to `<data>/operator/receipts.jsonl`, signed with the owner-only key at `<data>/operator/audit-key.json`; a wrong-scope grant gets 403 and a `scope_denied` receipt, and static-token operations write no receipt. Receipts carry the DID, scope, action, target, grant id, outcome, key id and signature, never the bearer token. No in-game gate or other player-facing feature reads this state.

M61/M62's game triggers read `<server_api_url>/v1/links/{uuid}` and `<server_api_url>/xrpc/com.atproto.label.queryLabels`, then sign the justified automatic labels through `POST <server_api_url>/v1/labeler/labels` with `server_admin_token`. The subject DID and label value are the only player data sent; no name, UUID, op level, statistic or other game state goes to the labeler.

M63's player-data and RPG commands read the same public `/v1/links/{uuid}` route to require that the target UUID is linked to the worker's logged-in DID. Capture uploads the five supported aspect blobs to `at.atpcraft.player.snapshot` in that account's PDS (`com.atproto.repo.uploadBlob` then a signed `createRecord`); restore reads the account's own public collection (`listRecords`/`sync.getBlob`) and applies verified bytes. RPG discovery sends no inventory contents off the machine: only the item key, registry id, display title and renamed flag go to the worker, which writes `equipment.rpg.give` as the server account and `equipment.rpg.item` as the player via `putRecord`. The server never receives another player's credentials.

M68's automatic hooks use exactly the same routes and gates as the manual commands. On leave the game captures the same five aspects and sends them with `automatic: true`; the worker drops the command without any network call when `player_data_sync_enabled` is off, or after the same `/v1/links/{uuid}` self-link check when it is on. On join the worker lists the account's snapshot collection, takes the highest version whose endorsement verifies against the local signing key or a `trusted_server_keys` peer, and downloads its aspect blobs; a peer server's signed snapshot therefore crosses servers without a new endpoint or credential. No full player NBT leaves or returns to the game, and no data is sent to any server other than the configured identity service and the player's PDS.

### Nothing else

No telemetry, no analytics, no endpoints beyond those listed above. The optional identity service above is a separate listener, not part of the game mod's network use.

## Layout

Rust source follows `<scope>/[<subscope>/]<atom>.rs`: directories are scopes, files are single-word atoms, and `mod.rs` only declares a scope. `crates/atmospheric`, `crates/atmospheric-server` and the mixin helper scopes are complete. `scripts/check-layout.sh` enforces atom naming plus scope placement and `mod.rs` declarations in both Rust crates.

- `mixins/atmospheric/` — horizon-mixins declarations: the injected std-only `runtime.rs` (which also spawns and bridges the sidecar), its placeholder hook, `horizon-mixin.toml`, and helper atoms grouped into `colibri/`, `gate/` and `labeler/` scopes.
- `crates/atmospheric/` — the protocol library, organised into `account/` (`identity`, `oauth`, `plc`, `session`), `base/` (`config`, `error`, `records`, `signing`, `tid`), `social/` (Bluesky reads, writes, digests, the M59 labeler declaration and the M61/M62 triggers), `colibri/` and `worker/`. It covers M8 OAuth, M11/M15 lists, M12 labels, M13 presence, M14 skins, M16 chats, M19 servers, M20/M23 social graphs, M21 interactions, M24 profiles, M36–M40 identity and M6/M9 atpcraft records, plus the `atmospheric-worker` bin and its end-to-end test (`tests/sidecar.rs`).
- `crates/atmospheric-server/` — the optional HTTP identity service, organised into `api/`, `browser/`, `error/`, `labeler/`, `model/`, `oauth/`, `page/` and `store/` scopes: admin-auth router and handlers, link model and validation, atomically persisted store, browser OAuth, the M56–M58 label signing/log/query/stream layer, and integration tests.
- `docker/` — the server image, compose file and end-to-end Docker test.
- `oauth-client-metadata.json` — the OAuth client metadata document to host when an authorization server does not support the loopback client ([#21](https://github.com/ewanc26/atmospheric-horizon/issues/21)).
- `compatibility.json` — the pinned Horizon and horizon-mixins revisions the bundle is built against.
- `CHANGELOG.md` — release notes and the release process.
- `docs/ARCHITECTURE.md` — how the layers connect, for reviewers and contributors.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow and the checks required before opening a pull request. [AGENTS.md](AGENTS.md) carries the rules for AI coding agents.

## Licence

AGPL-3.0-only. See [LICENSE](LICENSE).

Copyright (C) 2026 Ewan Croft (ewanc26).

## Support

If you find this project useful, consider supporting its development:

[![Ko-fi](https://img.shields.io/badge/Ko--fi-F16061?style=for-the-badge&logo=ko-fi&logoColor=white)](https://ko-fi.com/ewancroft)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-30363D?style=for-the-badge&logo=github&logoColor=white)](https://github.com/sponsors/ewanc26)
[![Liberapay](https://img.shields.io/badge/Liberapay-A60000?style=for-the-badge&logo=liberapay&logoColor=white)](https://liberapay.com/ewanc26/)

## Star History

<a href="https://www.star-history.com/?repos=ewanc26%2Fatmospheric-horizon&type=date&legend=bottom-right">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=ewanc26/atmospheric-horizon&type=date&theme=dark&legend=bottom-right" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=ewanc26/atmospheric-horizon&type=date&legend=bottom-right" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=ewanc26/atmospheric-horizon&type=date&legend=bottom-right" />
 </picture>
</a>
