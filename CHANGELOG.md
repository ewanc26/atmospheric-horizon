# Changelog

All notable changes to atmospheric-horizon are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases are cut from `main` by pushing an annotated `vX.Y.Z` tag; the
[release workflow](.github/workflows/release.yml) then builds the
`atmospheric-worker` sidecar for Linux x86_64, macOS arm64 and Windows x86_64,
packs the mixins bundle, and attaches everything (with a `SHA256SUMS` file) to
the GitHub Release. See the release section of `docs/ARCHITECTURE.md`.

## [Unreleased]

## [0.10.0] — 2026-11-25

Tenth tagged release. Adds verified handle plaques — cryptographic handle verification from a fresh signed commit — with no new endpoint or credential.

### Added

- **Verified handle plaques (#185).** `/atmospheric verify <did|handle>` proves a handle belongs to the logged-in account: a throwaway `at.atpcraft.verify.nonce` record forces a fresh signed commit, whose signature is verified against the DID's PLC-anchored `#atproto` key, with TID freshness and bidirectional handle resolution. The plaque reports PDS repository custody, not human identity; no profile data is written.

## [0.9.0] — 2026-11-25

Ninth tagged release. Adds cosmetic Warrants: signed credential records accepted
only from pinned issuers, with no new endpoint or credential.

### Added

- **Warrants: cosmetic ranks from signed issuer records (#192).** Set
  `warrant_issuers` to pin the DID, collection and Ed25519 public key of the
  issuers you recognise, then `/atmospheric warrants <did|handle>` reads signed
  credential records from that player's own repo and shows the active ones as
  name-plate lines. A rank is accepted only when the signature verifies with
  the exact pinned key for that DID and collection and the subject matches;
  self-attested records are worthless. Deleting a record drops the rank on the
  next fetch and `expiresAt` can time-box a grant. Ranks are cosmetic only —
  never a join gate or gameplay capability. The read is unauthenticated and
  writes nothing; an empty allowlist trusts nobody and sends no record reads.

## [0.8.0] — 2026-11-25

Eighth tagged release. Adds Counting Stones polls: a signed tally root, private
voter receipts and a one-DID-one-vote worker lifecycle, with no new endpoint or
credential.

### Added

- **Counting Stones: verifiable in-game polls (#193).** The worker can open a
  ballot box, collect one vote per linked DID, close it with a single signed
  tally record, and hand each voter a private receipt that proves inclusion in
  the Merkle root. The published record carries the question, choices, root,
  participant DID list and aggregate tally — never a choice next to an
  identity — and a reader verifies its Ed25519 signature with the public key
  it carries. `/atmospheric poll open|vote|close|receipt|result` exposes the
  flow in game. No new endpoint or credential: votes resolve the existing
  public identity link, the write rides the logged-in PDS session, and the
  signing key stays in the worker.

### Changed

- **ADR-002 records the native server export (#209).** The architecture guide now records that Horizon already ships third-party native mods and the trusted standalone dedicated-server export, superseding ADR-001's upstream blocker. The `route-3`/`blocked-upstream` labels are removed from the parked issues, the README no longer describes registry distribution as blocked upstream, and the distinction between today's sidecar build and the future native server half is explicit. No runtime behaviour changes.

## [0.7.0] — 2026-11-25

Seventh tagged release. Adds configurable Bluesky, Spark and Grain image-post
targets through a new setting, with no new endpoint, AppView call or credential.

### Added

- **Configurable image-post targets (#183).** Set `image_post_target` to
  `bluesky` (default), `spark` or `grain` to choose where the `V` keybind and
  automatic F2 screenshot posts are written. Spark uses `so.sprk.feed.post`
  with its image media union and caption; Grain writes a linked photo, gallery
  and gallery-item trio. All three upload the image to the configured PDS with
  the existing `uploadBlob`/`createRecord` calls; no new endpoint, AppView
  call or credential. Target-specific record shapes, local caps, OAuth grants,
  settings/wire round-trips and worker PDS tests are included.

## [0.6.0] — 2026-11-25

Sixth tagged release. Adds two default-off automatic Bluesky posts — achievement
announcements and F2 screenshots — with no new endpoints, record kinds or
credentials.

### Added

- **Automatic screenshot posting (#182).** With the new default-off
  `post_screenshots` setting, each successful F2 screenshot is uploaded and
  posted with an image embed. The screenshot hook sends an `automatic` flag that
  the worker gates before any file or network I/O, so leaving the setting off
  costs nothing; the manual `V` keybind always posts the latest shot regardless.
  Wire-shape pins, a worker PDS test and a game-side command-flow test cover the
  new path. No new endpoint or credential.
- **Achievement announcements (#181).** With the new default-off
  `post_achievement_messages` setting, a player's advancement announcements
  (`chat.type.advancement.*`) are posted to Bluesky with a 🏆 prefix. The
  existing `system_message` injection now recognizes both death and
  advancement translation keys and routes them to separate worker commands,
  each gated by its own setting. Wire-shape pins, a worker PDS test and a
  game-side command-flow test cover the new path. No new endpoint or credential.

## [0.5.0] — 2026-11-25

Fifth tagged release. Adds automatic player-data sync and liveness-checked
server cards, updates the build and release tooling, and moves the crypto
dependencies onto their current major versions without changing any wire
format.

### Added

- **M68 — automatic player-data sync (#162).** With the new default-off
  `player_data_sync_enabled` setting, a linked player's inventory, ender chest,
  equipment, advancements and statistics are captured at
  `native_player_logging_out` (while the player is still in their level) and the
  newest verified snapshot is restored at `player_logged_in` — on this server or
  a `trusted_server_keys` peer. Hook-originated commands carry `automatic: true`
  and are silently skipped before any network call while the setting is off;
  the explicit `/atmospheric player-data` commands always run. No new endpoint,
  event or credential.
- **M69 — server cards (#191).** The server account publishes a typed
  `click.croft.atmospheric.server.card` record at rkey `self`, stamped with a
  fresh `createdAt` on every publish, so a card's liveness is its author's
  recency (`Fresh` within an hour, `Stale` after, `Unknown` for an unparseable
  timestamp) — not anyone's endorsement. The record carries no signature
  block: repo authentication is the authorship claim, and a signature proves
  authorship, not truth. `/atmospheric server-card publish` publishes on demand
  from the live player count; a server republishes every five minutes, gated
  on a player-slot ceiling so clients stay silent. `/atmospheric card-of
  <did|handle>` reads another server's card unauthenticated and shows its name,
  version, population, pack count and liveness. Identity fields come from the
  settings file and `engineRevision` from `compatibility.json`; packs declare
  none yet because Horizon's signed-registry checksum lookup is a later slice.
  The record, its read path and the game-side mirror are validated and pinned
  by wire-shape tests. No new endpoint or credential.

### Changed

- **Package metadata.** The crate manifests now list their authors, and the
  README states the copyright holder (#180).
- **Crypto dependencies.** `ed25519-dalek` 3 and `k256` 0.14 arrive together
  with `rand_core` 0.10; key generation now uses `getrandom` 0.4's `SysRng`
  wrapped in `UnwrapErr`, and the label signer uses `Generate::generate`,
  `to_sec1_point` and the value-returning `normalize_s` (#211). Ed25519 SPKI
  key ids and compact low-S secp256k1 signatures remain byte-identical.
- **Other dependencies.** `base64` 0.23 (#169), `sha2` 0.11 (#171),
  `tower-http` 0.7 (#172), `actions/upload-artifact` 7 (#166),
  `actions/download-artifact` 8 (#167) and `softprops/action-gh-release` 3
  (#165).
- **CI diagnostics.** Clippy output is colourless, filtered to diagnostic
  lines and mirrored onto the PR, with a Rust problem matcher registering the
  annotations.

### Fixed

- Aligned the runtime's hand-rolled snapshot summary JSON with the worker's
  serde struct (`xpLevel`/`gameMode`), with runtime encode and worker decode
  tests pinning the exact wire shape. Full player-NBT restore remains out of
  scope because Horizon has no complete round-trip setter for the player
  compound.

## [0.4.0] — 2026-11-20

Fourth tagged release. Adds the generic record browser, linked-player names and
hosted lexicons, chat and server-profile parity, proactive token refresh and
automatic server-profile publish, a list-member cache, configurable OAuth scope,
grants and browser-link lifetime, a complete labeler (signed labels, a durable
log, public query/stream endpoints, a service declaration and PLC identity
publication, game triggers and op-level/statistic labels), player-data aspect
capture and RPG item discovery, browser skin upload, and profile editing with
avatar and banner uploads (M44–M66).

### Added

- **M44 — generic record browser (#107).** `/atmospheric records <did|handle>
  <collection>` reads any public collection from the subject's own PDS with an
  unauthenticated `com.atproto.repo.listRecords` and shows the total plus the
  newest five records as compact JSON in the HUD (collapsed to one line and
  truncated). Reuses the M13/M16 read path: no new endpoint and no new stored
  data.
- **M45 — linked-player display names and pronouns (#109).** A linked player's
  Bluesky display name and pronouns are applied as their in-game name when they
  join a server (plain text in chat and the player list), using the profile
  reads already used by `/atmospheric profile`. Unlinked players and failed
  reads leave the vanilla name alone; no new endpoint, credential or stored
  data.
- **M46 — lexicon documents (#114).** `atmospheric-server` publishes the five
  owned `at.atpcraft.*` lexicon documents at `/lexicons` and
  `/lexicons/{nsid}.json`, with an HTML index and permissive CORS. The schemas
  match the records the mod writes and are compiled into the binary; no
  filesystem read, outbound request, credential or stored player data is added.
- **M47 — atpcraft-compatible linked-player names (#116).** A linked player's
  display name is reduced to a valid 16-character Minecraft name (keep
  `[A-Za-z0-9_]` and whitespace, collapse whitespace to `_`, strip the rest and
  cap), with the handle's username as the fallback; the plain-text pronoun
  suffix remains because the pinned helper set has no chat-only styled hook.
  Unchanged and unusable names leave the vanilla name alone. No new endpoint,
  credential or stored data.
- **M48 — signing-key chat server id (#118).** Chat records now default their
  `server` field to the worker's persisted Ed25519 key id, matching atpcraft's
  `signingKey.keyId`; `chat_server_id` remains an optional override. The key is
  loaded or created through the existing snapshot path on first chat. No new
  endpoint, credential or record shape.
- **M49 — server-profile name and capabilities (#120).** A blank `server_name`
  now falls back to `client_name` (`atmospheric-horizon` by default), matching
  atpcraft's client-name fallback. `server_capabilities` is an optional
  override; when empty, the descriptor advertises `skin-sync` and, when the
  gate is armed, `join-gate`. It no longer claims `player-data-sync` before
  server-side capture/restore exists. No new endpoint, credential or record
  shape.
- **M50 — proactive server-account token refresh (#122).** The worker checks
  the server account's OAuth token on `server_account_token_refresh_seconds`
  (default 60, minimum 1) instead of refreshing only when a server-side action
  needs it. The existing expiry check keeps a healthy token an in-memory check;
  a completed server login resets the cadence. No new endpoint, credential or
  record shape.
- **M51 — automatic server-profile publish (#124).** A successful
  server-account login, and a persisted server session resuming at startup,
  now re-publish `at.atpcraft.server.profile` through the existing writer. The
  write is best-effort and keeps the manual `/atmospheric server-account
  publish` command; no endpoint, credential or record shape changes.
- **M52 — cached list members (#126).** `list_cache_seconds` (default 300;
  `0` always refreshes) caches `app.bsky.graph.getList` members by AppView and
  canonical list. A fresh hit makes no request, a stale cache refreshes once,
  and a fresh gate miss refreshes once before denying. Explicit list reads,
  gate decisions and the sweep reuse the cache; failures stay fail-closed and
  keep the last successful cache. No endpoint or record shape changes.
- **M53 — configurable OAuth scope (#128).** `oauth_scope` replaces the
  requested base scope (empty keeps `atproto transition:generic`), and the
  Colibri repo/RPC grants are still appended — once each — when the bridge is
  enabled. A hosted client-metadata document must declare a custom scope. No
  endpoint, credential or record shape changes.
- **M54 — required OAuth grants (#130).** A narrow `oauth_scope` now gains the
  snapshot, skin, chat and `blob:*/*` grants the implemented writes need, and
  server-account logins add the server-profile grant plus the Colibri stream
  grant when the bridge is on. Player logins no longer request the stream
  grant. The default `atproto transition:generic` string is unchanged. No
  endpoint, credential or record shape changes.
- **M55 — configurable browser-link lifetime (#132).** `browser_link_ttl_seconds`
  (default 600) is sent as the optional `ttl_seconds` field when the game server
  creates a browser link, and the server applies its existing 60–3600-second
  pairing bound to the stored expiry. The join-gate refusal reports the
  configured window. An omitted field keeps the service default. No endpoint or
  record shape changes.
- **M56 — label signing core (#134).** Add the server-side `labeler` scope:
  the `com.atproto.label.defs#label` wire type, canonical DAG-CBOR encoding of
  the unsigned label, and K-256 key handling with multibase `did:key` plus
  64-byte low-S signatures. This is the first slice of #112; no endpoint, PLC
  update, declaration record, label store or game trigger is added yet.
- **M57 — label log and queryLabels (#136).** Add the server-side labeler
  state: owner-only K-256 key persistence, an append-only JSONL label log with
  sequence recovery, an active-state index rebuilt on restart, the admin
  `POST /v1/labeler/labels` signing/append route, and the public
  `GET /xrpc/com.atproto.label.queryLabels` route with patterns, sources,
  expiry and cursor pagination. The `app.bsky.labeler.service` declaration,
  PLC identity update, `subscribeLabels` stream and game triggers remain in
  #112.
- **M58 — subscribeLabels and public CORS (#138).** Add the public spec
  `GET /xrpc/com.atproto.label.subscribeLabels` WebSocket. Each label is a
  binary DAG-CBOR frame (`{t:"#labels", op:1}` + `{seq, labels}`), with `sig`
  as a byte string. A `cursor` backfills the durable log before live fan-out;
  a cursor past the tail returns a `FutureCursor` error frame and a normal
  close. The public XRPC routes also answer permissive CORS for browser
  consumers. No environment variable, record shape or outbound request is
  added.
- **M59 — labeler service declaration (#140).** Add `labeler_enabled` (default
  false). A successful server-account login and a persisted session resuming at
  startup upsert `app.bsky.labeler.service` at rkey `self`, preserving
  `createdAt`, declaring the 11 label values with their client metadata. A
  narrow server-account OAuth scope gains the declaration write grant, and the
  server profile advertises `labeler` when the flag is on. No PLC identity
  update or game trigger is added, so the labeler is not yet discoverable
  through the DID document. The record is stored on the PDS, not locally.
- **M60 — labeler PLC identity publication (#142).** `atmospheric-server` serves
  the labeler's public identity at `GET /v1/labeler/identity` (service DID,
  signing `did:key`, public endpoint). `/atmospheric labeler-identity status`,
  `request` and `submit <token>` read the account's current PLC operation,
  preserve every existing rotation key, alias, verification method and service,
  add `#atproto_label` and `#atproto_labeler`, and sign/submit the operation as
  the server account. A narrow server-account scope gains `identity:*` while
  the labeler is enabled. No new local storage or label emission.
- **M61 — labeler game triggers (#144).** With `labeler_enabled` and the new
  `server_admin_token`, server join/logout/sweep hooks assert a linked player's
  rolling `player` badge (30-day expiry, refreshed at most once a day) and
  `online` label; logout negates `online`; a once-a-minute sweep corrects
  online players, and startup negates stale `online` labels after an unclean
  stop. Unlinked players are skipped, and automatic failures stay silent.
- **M62 — op-level and vanilla-stat labels (#146).** A join forces the next
  tick's label sweep, which captures each online player's op level and vanilla
  playtime, mob kills, deaths and Ender Dragon kills. The worker asserts
  `admin` (op level at least 1), the highest playtime tier (10/100/1000 hours),
  `dragon-slayer`, `monster-hunter` and `death-defier`, and negates labels the
  current state no longer justifies. Manual `vip`/`donor` grants are never
  touched, and only label values leave the game.
- **M63 — RPG discovery and player-data aspects (#111).** Add server-tick
  `/atmospheric player-data capture|restore [player]` for gzip-NBT
  `inventory`, `ender_chest` and `equipment` plus raw `advancements`/`stats`
  JSON, and `/atmospheric rpg discover [player]` for item statistics and
  renamed stacks. Capture/restore require the target UUID to be linked to the
  worker's logged-in DID; restore verifies the latest snapshot before applying
  anything. RPG discovery excludes wearables and, when `rpg_items_enabled` is
  on, writes deterministic `equipment.rpg.give`/`equipment.rpg.item` pairs from
  the server and player accounts. Core-NBT aspects and automatic sync remain
  future work. atpcraft was used only as a wire-behaviour reference; no code was
  copied.
- **M64 — browser skin upload (#113, partial).** Add `/atmospheric skin upload`
  for already-linked players: the game server requests a one-time
  `/skin/{token}` URL from `POST /v1/browser/skins`, and the browser signs in
  with Bluesky, uploads a PNG (at most 1 MiB), and the service publishes the
  `at.atpcraft.player.skin` record with the worker's Ed25519 endorsement,
  preserving the cape and other servers' endorsements. Add the optional
  `ATMOSPHERIC_SERVER_SKIN_KEY` path (the game's `signing-key.json`), a
  `/skin/callback` OAuth redirect, and the `StartSkinUpload`/`SkinUploadLink`
  protocol pair. In-game application remains blocked because the pinned
  Horizon API has no skin/texture setter; atpcraft was used only as a wire
  reference.
- **M65 — profile-field editing (#150).** Add `/atmospheric profile-set
  <name|description|pronouns> [text]` for the logged-in account. The worker
  fetches `app.bsky.actor.profile` at rkey `self`, merges the field, and
  upserts it with PDS validation, preserving the avatar, banner, links,
  pinned post, `createdAt` and any other field. Empty text clears the field.
  A narrow OAuth scope gains the `app.bsky.actor.profile` write grant. Live
  checks against a real PDS update and restore a profile field.
- **M66 — profile avatar and banner uploads (#152).** Add
  `/atmospheric profile-set avatar|banner <path>` for the logged-in account.
  Local PNG/JPEG files up to 1,000,000 bytes are uploaded as blobs, merged
  into `app.bsky.actor.profile` at rkey `self`, and written with PDS
  validation, preserving every other field. The existing `blob:*/*` grant
  covers the uploads. Live checks uploaded both images and restored the
  original images.

## [0.3.1] — 2026-11-14

Small tagged release for the M42 online whitelist sweep (#103): the armed join gate now re-reads `join_gate_list` every `join_gate_sweep_seconds` (default 60; `0` disables; a positive value is floored at 60) and disconnects a linked player whose DID has left it. No new endpoint or stored data — version pins and docs only.

## [0.3.0] — 2026-11-14

Third tagged release. Adds the social-graph write side, profile reads, a
separate server account, the Colibri chat bridge and its follow-ups, and the
optional server-side identity service with in-game and browser linking; also
migrates both Rust crates to the scope/atom source layout.

### Changed

- **Protocol scope layout.** Move the protocol modules under `account/`,
  `base/`, `social/`, `colibri/` and `worker/` scopes. Crate-root re-exports
  preserve every public module path, and the layout guard now enforces scope
  placement in both Rust crates.

### Fixed

- **Injected notice helpers (#92).** Keep `Text::component()` values alive in
  `colibri/notice.rs` and `death.rs`, so the mixin bundle compiles against the
  pinned Horizon revision.

### Added

- **M40 — browser link from the gate refusal.** An unlinked player refused by
  the M38 join gate now gets a one-time browser URL on the disconnect screen,
  so they can link and reconnect without an operator code. The worker creates
  the link through the existing M39 endpoint inside the gate's 5-second
  budget and falls back to the generic refusal if creation fails. The joining
  player's name is carried on `JoinGateCheck` for that request. A new
  router-level OAuth integration test exercises link creation, the browser
  form, PAR, PKCE, DPoP, the callback and persistence. `docker/.env.example`
  and the README now provide a copyable Docker plus `config.json` setup.
- **M39 — browser self-service linking.** `/atmospheric link` without a code
  asks the identity service for a one-time browser URL, authenticated with the
  new server-only `server_link_token`. The service publishes OAuth client
  metadata, runs PAR + PKCE S256 with a DPoP key, validates the callback
  issuer/state/DID/scope, and binds the signed-in DID to the UUID the game
  server supplied. Link tokens and OAuth state expire after ten minutes and
  are consumed once; they are not persisted. `/atmospheric link <code>` still
  claims an operator-issued pairing code. The service now makes outbound
  requests to the handle resolver, PDS and authorization server for this flow.
- **M38 — linked-account join gate (#91).** `join_gate_enabled` and
  `join_gate_list` arm a login-phase gate. Horizon's `player_login` hook
  waits, within a bounded budget, for the worker to resolve the joining UUID
  through `server_api_url` and check the linked DID against the public list.
  A linked member is admitted; unlinked players, non-members, an empty list,
  a failed read and a timeout are refused with a disconnect reason. The
  worker publishes the gate state at startup and after settings saves. This
  is enforcement for already-linked accounts; M39 adds the browser linking
  flow and M40 puts it on the refusal screen, while the online whitelist
  sweep remains separate.
- **M23 — social-graph writes (#60).** `/atmospheric follow`, `unfollow`,
  `block`, `unblock`, `mute` and `unmute` take a DID or handle. Follows and
  blocks are TID-keyed `app.bsky.graph.follow` / `block` records in your own
  repo; undoing them lists the matching record and deletes it. Mutes use
  `app.bsky.graph.muteActor` / `unmuteActor`. Self-actions are refused, and
  success reports a `GraphChanged` HUD line.
- **M24 — profile reads (#63).** `/atmospheric profile <did|handle>` reads
  the AppView's `app.bsky.actor.getProfile` and the direct
  `app.bsky.actor.profile` record at rkey `self`, unauthenticated. The direct
  record's pronouns win when that read succeeds, even when the field is
  absent; a failed direct read falls back to the AppView value, matching
  atpcraft's precedence. The HUD shows handle, display name, pronouns and a
  truncated description.
- **M25 — server account (#65).** `/atmospheric server-account login
  <handle>|status|logout|publish` manages a second, separate OAuth account for
  server-side work. Its session lives in `server-session.json`, resumes at
  startup, refreshes independently of the player and can publish the server
  profile as the server account. The existing `/atmospheric server` still
  publishes as the player account.
- **M26 — Colibri bridge core (#67).** `/atmospheric colibri
  start|stop|status|send <text>` parses a channel AT-URI or app link, mints a
  service-auth token from the server account, holds the Colibri `subscribeEvents`
  WebSocket, and shows channel upserts in native chat. Game chat is mirrored
  into the channel as the player account while the bridge runs. The Colibri
  OAuth grants are added only when `colibri_bridge_enabled` is set.
- **M27 — Colibri membership and token rotation (#69).** Before opening the
  stream, the worker lists the server account's `social.colibri.membership`
  records and creates one for the bridged community when absent, using
  `createRecord` so a second start creates no duplicate. The socket closes at
  45 minutes and the worker mints a fresh service-auth token and reconnects,
  keeping the one-hour token ahead of expiry.
- **M28 — Colibri server event notices (#71).** The server-side
  `broadcast_message` hook classifies translation keys and forwards join,
  leave, death and advancement lines to the worker, which writes them into the
  bridged channel as the server account. `colibri_server_events_enabled`
  defaults to true; ordinary chat and unknown lines are ignored.
- **M29 — Colibri presence fallback and approval notice (#73).** Server-side
  `player_logged_in`/`player_logged_out` hooks schedule a fallback notice when
  a chat mod rewrites or hides the vanilla join/leave line. The worker waits
  3 seconds and writes only when no matching or blank line arrived within 5
  seconds; blank lines suppress the fallback. `colibri_server_events_fallback_enabled`
  defaults to true. After a new membership is created, the worker reads the
  community's `requiresApprovalToJoin` flag unauthenticated and shows a HUD
  notice when a moderator must approve the join. The server-wide profile prompt
  remains.
- **M30 — Colibri local profile prompt (#75).** When a Colibri bridge starts
  and the local player is logged in, the worker reads the player's
  `social.colibri.actor.profile` at rkey `self`. If it is absent, native chat
  shows atpcraft's two-line setup prompt with `https://colibri.social`; an
  existing profile or a failed read stays quiet. M37 later adds the check for
  other linked players.
- **M31 — Colibri reconnect and send throttle (#77).** `/atmospheric colibri
  reconnect` requires an active bridge, re-mints the server account's
  service-auth token and restarts only the event stream; the channel, echo
  guard, presence generation and profile-check state survive. Player-authored
  writes are capped at 20 per rolling 60 seconds, matching atpcraft's
  `ColibriRateLimiter`; server notices are exempt and automatic chat-hook
  sends drop silently at the limit.
- **M32 — local Colibri player membership (#79).** When the bridge is active,
  the worker ensures the local player's own `social.colibri.membership` record
  exists, both on bridge start and after a player login. A new record in an
  approval-gated community shows atpcraft's pending line in chat and the HUD;
  existing memberships and failed writes stay quiet. The once-per-bridge guard
  resets on explicit stop and player logout/login.
- **M33 — Colibri incoming chat hygiene (#81).** Incoming Colibri text now
  matches atpcraft's formatter: control and colour characters become spaces,
  whitespace runs collapse, the result is trimmed, and handles/text are capped
  at 64/512 characters. This prevents adjacent words from merging when a
  colour or control character is removed.
- **M34 — Colibri status detail (#83).** `Event::Colibri` now carries the owning
  community URI, filled from `ChannelRef::community_uri()`. The connected,
  connecting and disconnected status lines show both the channel and community,
  matching the detail atpcraft's `/atpcraft colibri status` prints.
- **M35 — server-side identity service (#85).** A new optional
  `atmospheric-server` binary maintains the shared Minecraft UUID-to-DID map
  that the client cannot build alone. Public reads resolve links; admin-token
  writes create, replace and delete them; one-time pairing codes let players
  claim a link without the admin credential. Links and pending codes persist
  atomically in `links.json`, and the image has a Docker end-to-end test for
  authentication, validation, pairing, deletion and restart persistence. M36
  adds the in-game claim, M37 the join-time profile check, M38 linked-account
  join enforcement, M39 browser self-service linking and M40 the link on an
  unlinked gate refusal; label issuance, RPG discovery and aspect capture
  remain unimplemented.
- **M36 — in-game identity link claim (#87).** `/atmospheric link <code>`
  claims an operator-issued M35 pairing code as the logged-in player. The
  worker posts only the code, DID and optional handle to the configured
  `server_api_url`, rejects a response for a different DID, and reports the
  linked player name and UUID in the HUD. No request is made unless the command
  is used; no admin token, session token or game state is sent.
- **M37 — server-wide Colibri profile checks (#89).** The server-side
  `player_logged_in` hook now carries the joining player's UUID to the worker.
  While the bridge is active, the worker resolves the UUID through the public
  `GET <server_api_url>/v1/links/{uuid}` and reads the linked account's
  `social.colibri.actor.profile` at rkey `self`, unauthenticated. A missing
  profile sends atpcraft's two-line setup prompt to that player with
  `native_tell`; unlinked players, a missing service URL, failed reads and the
  local player (already covered by M30) stay quiet. Server ticks now attach the
  sidecar and drain events, so the prompt does not require a client window.

### Changed

- **Horizon compatibility bump (#8).** The pinned Horizon revision moves from
  `ba6344a` to `9f42064` (nightly 2026-11-13). That revision carries
  horizon-rust/horizon#9394, so `horizon-ctl command "/atmospheric …"` now
  reaches the mod's client-side `command` hook instead of the integrated
  server console; the chat-box key workaround is no longer needed. The
  pinned source hashes and the mixin's `[compatibility]` revision move with
  it.

## [0.2.0] — 2026-11-13

Second tagged release. Adds the read side of the server descriptor, public
social-graph reads, and the three basic post interactions; also brings the
repository templates and documentation in line with the sibling projects.

### Added

- **M19 — server-profile reads (#44).** `/atmospheric server-of <did|handle>`
  pages `at.atpcraft.server.profile` on the subject's own PDS, prefers the
  `self` record, and reports the server name, Minecraft version,
  capabilities, operator and whether the published signing key is trusted,
  unknown or mismatched.
- **M20 — social-graph reads (#48).** `/atmospheric follows|followers|blocks|mutes
  <did|handle>` pages `app.bsky.graph.getFollows`, `getFollowers`,
  `getBlocks` or `getMutes` on the public AppView and reports the count plus
  the first handles in the HUD. Unauthenticated, so it works logged out.
- **M21 — interactions (#53).** `/atmospheric reply <at-uri> <text>`,
  `/atmospheric like <at-uri>` and `/atmospheric repost <at-uri>` read the
  target post's CID (and thread root) with an unauthenticated
  `com.atproto.repo.getRecord`, then publish `app.bsky.feed.post` (with
  `reply` strong refs), `app.bsky.feed.like` or `app.bsky.feed.repost` to your
  own repo. bsky.app post URLs are accepted too.

### Changed

- **Repository conventions (#50).** The shared bug/feature/documentation issue
  forms, a pull-request checklist, `.gitattributes`, and the sibling README
  Contributing/Star History sections.

## [0.1.0] — 2026-11-13

First tagged release. The mod is an early but complete client-side port of the
atpcraft feature set to Horizon: all disk and network I/O lives in the sidecar
worker, the injected runtime stays std-only, and every read is unauthenticated
unless it writes to your own repo.

### Added

- **M1/M2 — protocol library and worker wiring.** A game-free `atmospheric`
  crate (ATrium) behind a command/event channel, with the injected runtime
  owning the keybind, `/atmospheric` commands and a HUD status line.
- **Login (M2, M8).** App-password login
  (`com.atproto.server.createSession`) as the fallback, and localhost OAuth
  with PAR, PKCE, DPoP-bound access tokens, a loopback redirect and
  `oauth-client-metadata.json` for authorization servers without the localhost
  client (M8, #21). Sessions resume across restarts.
- **Posts (M2/M3).** Text posts and screenshot posts (F2 then the post keybind,
  `com.atproto.repo.uploadBlob` + `app.bsky.embed.images`), plus opt-in death
  messages.
- **Notifications (M4).** Timer-driven `app.bsky.notification.listNotifications`
  polling with a persisted seen-cursor and a HUD digest.
- **atpcraft chat mirror (M5/M16).** Chat lines as TID-keyed
  `at.atpcraft.player.chat` records when enabled, `/atmospheric chat` for
  explicit posts, and `/atmospheric chat-of <did|handle>` to read any
  account's history.
- **Snapshots and presence (M6/M13).** Ed25519-endorsed
  `at.atpcraft.player.snapshot` records with a monotonic version, and
  `/atmospheric player <did|handle>` to read the highest-version record from
  another account's PDS and verify it against your key or
  `trusted_server_keys`.
- **Skins (M9/M14).** Skin/cape PNG blobs published as an endorsed
  `at.atpcraft.player.skin` record via `/atmospheric skin`, and
  `/atmospheric skin-of <did|handle>` to verify another account's record and
  download only verified blobs.
- **Server profile (M10).** `/atmospheric server` publishes
  `at.atpcraft.server.profile` at rkey `self`, preserving `createdAt`.
- **Join-gate reads and decision (M11/M15).** `/atmospheric list` pages
  `app.bsky.graph.getList` on the public AppView; `/atmospheric gate` decides
  membership fail-closed (enforcement stays server-side).
- **Label reads (M12).** `/atmospheric labels` reads AppView `labels` fields;
  `/atmospheric labeler` queries a labeler's `#atproto_labeler` endpoint
  (`com.atproto.label.queryLabels`). Label issuance stays server-side.
- **M17 — sidecar worker.** The game spawns the `atmospheric-worker` binary
  found next to the game executable (or `ATMOSPHERIC_WORKER_BIN`) and bridges
  commands/events over stdio JSON-lines, so the injected module can stay
  std-only without linking AT Protocol dependencies into the game build.
- **Release pipeline.** This changelog, a `v*`-tag release workflow with
  per-OS worker archives and the mixins bundle, and a runtime test pinning
  `VERSION` to the package version.

### Security

- Credentials, sessions and the signing key are written owner-only (0600 on
  unix); app passwords are never persisted, and OAuth refresh tokens are
  DPoP-bound.
- The mod talks only to the PDS and public services listed in the README's
  "Network use" section; there is no telemetry.
- Skin downloads are content-checked (PNG, size-capped) and only happen for
  records whose endorsement verifies.

### Known limitations

- Registry submission (M7, #28) is blocked on launcher support for third-party
  native-mod bundles; the `horizon-ctl` `command` bypass (#8) is upstream
  horizon-rust/horizon#15691.
- Label issuance remains a server concern (#29). Join-gate enforcement is
  available for linked accounts (M38); browser-based self-service linking is
  available while the player is connected (M39) and an unlinked gate refusal
  can issue the same link (M40). The online whitelist sweep remains to be
  implemented.

[Unreleased]: https://github.com/ewanc26/atmospheric-horizon/compare/v0.10.0...HEAD
[0.10.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.10.0
[0.9.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.9.0
[0.8.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.8.0
[0.7.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.7.0
[0.6.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.6.0
[0.5.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.5.0
[0.4.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.4.0
[0.3.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.3.0
[0.2.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.2.0
[0.1.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.1.0
