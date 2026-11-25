# Architecture

How atmospheric-horizon is put together, for reviewers, contributors, and coding agents. Start with `AGENTS.md` for the working rules.

## Architecture decision records

- **ADR-001 — native server half ([#208](https://github.com/ewanc26/atmospheric-horizon/issues/208), superseded).** Chose a native, unsandboxed server half for all server-side AT Protocol egress and server-DID custody, but assumed Horizon had no native server path. The decision to own server-side egress in-process stands; the premise about upstream support does not.
- **ADR-002 — adopt the existing native server export ([#209](https://github.com/ewanc26/atmospheric-horizon/issues/209)).** Verified against the fork's `db7d9029` sync that `horizon-launcher native-build FOLDER --server-output DIR --version TAG --trust-native-code` already exports a standalone dedicated-server engine with the pinned prebuilt SDK, an explicit native-code trust gate, `native-mods.json` provenance, `engine.path` deployment and rollback, server-only hooks and `Dist::DedicatedServer`, plus `native-mod` registry entries identified by source repo with SHA-256-pinned downloads and revocation. No upstream work is needed, and the parked server-publishing issues are schedulable. The target remains Route 3; the shipped code still uses the M17 sidecar until the native server half lands. Route 2 client-edge verification remains the degraded path when no trusted export is present, and the client native half is unchanged.

## Operator's Warrant (#189): DID grants stay inside the sidecar

The experimental DID-authenticated admin grants are an additive layer in the
optional `atmospheric-server` HTTP process. They use this repository's `/v1`
surface — `POST /v1/operator/challenge`, `POST /v1/operator/grant`,
`GET /v1/operator/audit-key` and `GET /v1/operator/receipts` — and are **not**
new Horizon `/api/v1` routes. The static `ATMOSPHERIC_SERVER_TOKEN` keeps
working, unchanged, on every admin route: grants are a second, short-lived,
scoped bearer credential, and static-token operations write no receipts.

The layer is deliberately not in the sandboxed `.hmod`. The injected
`mixins/atmospheric/runtime.rs` stays std-only and has no DID, HTTP-listener or
audit code, and the WASI/sandbox side has no sockets to accept these
connections. Under ADR-002 the future native server half and the client-native
Route 2 verification path are untouched: no Horizon stable API change, no new
hook and no change to the native export. The M17 sidecar remains the only
process that hosts this surface today.

The loopback/local-socket, token-gated control-plane assumption is preserved:
all DID authentication happens inside the sidecar process boundary, while the
game/worker channel is still plain JSON over a local pipe. The proof reuses
#185's PLC-anchored commit chain — the operator writes the throwaway nonce
record through their own PDS session and the sidecar makes unauthenticated
verification reads plus the existing handle/DID resolution. That proves PDS
repository custody, not human identity. Challenges and grants are in memory
only and do not survive a restart; no player-facing gate or gameplay feature
reads any of this state. Grant-exercised admin operations append signed receipts
to `<data>/operator/receipts.jsonl` under the owner-only key
`<data>/operator/audit-key.json`, including a `scope_denied` receipt for a valid
grant with the wrong scope. Keep that key private, and keep grant lifetimes
short: a leaked grant is a bearer token until it expires, and compromise of an
allowlisted DID's PDS key is effectively admin takeover.

## The two client layers

```text
┌──────────────────────────────┐   GameCommand channel   ┌──────────────────────────┐
│ Horizon game thread          │ ───────────────────────▶ │ worker thread            │
│ ┌────────────────────────┐   │                          │ tokio runtime            │
│ │ mixins/atmospheric/    │   │◀─────────────────────── │ ATrium agent / reqwest   │
│ │ runtime.rs (std-only)  │   │   ModEvent channel      │ ⇄ the PDS you configure   │
│ │ hooks + channels       │   │                          │ <data dir>/config.json   │
│ └────────────────────────┘   │                          │ (session, cursor in M3+) │
└──────────────────────────────┘                          └──────────────────────────┘
```

The optional `atmospheric-server` binary is not in this diagram: it is a separate HTTP process run by the Minecraft server operator, with no game imports. It owns the shared player UUID-to-DID map described under M35 below; browser linking adds the outbound OAuth requests described under M39, and M46 publishes the public `at.atpcraft.*` lexicons.

## Source layout

All Rust source follows `<scope>/[<subscope>/]<atom>.rs`: the directory names the domain scope, the final file is a single-word atom, and `mod.rs` only declares the scope. The injected module and Cargo entry points keep their required paths, and crate-root re-exports preserve the public protocol module paths. `scripts/check-layout.sh` is the dependency-free CI guard: it rejects non-atom Rust filenames everywhere, requires scope placement and `mod.rs` in both Rust crates, and keeps explicit exceptions for Cargo target paths such as `atmospheric-worker.rs`.

`crates/atmospheric-server` uses the same rule: `api/` holds the router, state and handler atoms (including M46's `lexicon` handler); `browser/` the one-time link state; `error/` the HTTP error mapping; `labeler/` the M56–M58 signing, key, log, state, query and stream atoms; `model/` the wire types and validation; `oauth/` the client, DPoP, resolver and encoding atoms; `page/` the browser HTML; and `store/` the in-memory store plus atomic file persistence. The compile-time lexicon documents live in `crates/atmospheric-server/lexicons/`. The public `app`, `ServerConfig`, `model`, `oauth` and `store` paths are unchanged.

`crates/atmospheric` now follows the same rule: `account/` holds `identity`, `oauth`, `plc` and `session`; `base/` holds `config`, `error`, `records`, `signing` and `tid`; `social/` holds the Bluesky read/write and digest atoms; `colibri/bridge.rs` holds the bridge; and `worker/service.rs` owns the worker. Public paths such as `atmospheric::oauth`, `atmospheric::records` and `atmospheric::worker` are unchanged.

The mixin helpers now follow the same rule: `colibri/` holds `join`, `leave` and `notice`; `gate/` holds `login`; the remaining helpers are single-word atoms at the mixin scope root.

Entry points (`lib.rs`, `main.rs`), the injected `runtime.rs` and the Cargo target files keep their required paths.

### The injected module: `mixins/atmospheric/runtime.rs`

This exact source is injected by horizon-mixins into the game as
`crate::game::atmospheric`, and it is also the standalone Cargo library of the
root package. Both builds must work, so it stays **std-only**: no external
crates, no game imports. Its only job is channel plumbing:

- `install()` creates the `GameCommand`/`ModEvent` channel pair in-process,
  keeps the game side in statics, and hands the worker side to whoever spawns
  the worker at startup (tests; a future `[[crate]]` glue).
- `ensure_attached()` (M17, called from the `tick` helper) spawns the
  `atmospheric-worker` sidecar binary found next to the game executable (or
  via `ATMOSPHERIC_WORKER_BIN`) and bridges the channel pair to the child's
  stdio — the shipped in-game attach path, see "M17" below.
- Hook helpers push `GameCommand`s through the sender and never panic if
  the worker is gone. The M38 login hook is the one bounded exception: it
  waits for the worker's gate decision, because `player_login` is
  synchronous. M2 hooks Horizon's named native hook points
  (`native-hooks.json` at the pinned revision): `mods` (the Options > Mods
  row and its keybind), `key` (the keybind posts the player's location),
  `command` (`/atmospheric login`, `/atmospheric post <text>`), `tick`
  (attaches the sidecar once, then drains events once per tick on the game
  or server), `player_login` (M38's gate veto) and `hud` (draws the status
  line in the bottom-left corner).
- `drain_events()` is called once per tick to collect worker events, folding
  each into the HUD status line (`status_line()`), bounded so the game
  thread never stalls. It also caches the worker's M38 gate state and
  decisions. Colibri message events are also queued for
  `drain_colibri_chat()`, which the tick hook prints to native chat when a
  window exists. M37's remote profile prompts use a separate queue and the
  tick hook sends them with `native_tell`, so a server tick can prompt a
  joining player without a window.
- The server-side `broadcast_message`  injection (`colibri/notice.rs`) reads
  the line's translation key and forwards only join/leave/death/advancement
  lines to the worker, which writes them as the server account.

### The protocol library: `crates/atmospheric`

Everything else, with zero Horizon imports so it builds and tests with plain
`cargo test`:

- `config` — settings, stored under the mod's own folder
- `session` — `AtpAgent` construction and app-password login
- `actions` — `create_record` posts (text, and image posts with an
  `app.bsky.embed.images` embed after `uploadBlob`), plus the M23 record
  delete and mute/unmute calls; MIME sniffing and PNG dimensions are pure
  helpers here
- `notifications` — polling with a persisted seen-cursor (dedup across
  restarts, the pattern bsky-bot-sdk uses)
- `lists` — M11 public list reads (`app.bsky.graph.getList` pagination) and
  the M15 join-gate decision (`check_gate`)
- `graph` — M20 social-graph reads (`app.bsky.graph.getFollows`/
  `getFollowers`/`getBlocks`/`getMutes` pagination on the public AppView,
  reduced to DID/handle/display name) and M23 writes (`follow`/`block`
  records, their removal, and the expected record shapes)
- `identity` — M36–M40 client for the optional M35 service: claim URL and
  request/response shapes, public UUID link lookup, browser-link creation,
  DID ownership check and service-error propagation
- `interactions` — M21 replies, likes and reposts (unauthenticated
  `com.atproto.repo.getRecord` for the target's strong ref and thread root,
  then `app.bsky.feed.post`/`like`/`repost` records in your own repo)
- `chats` — M16 chat-history reads (`at.atpcraft.player.chat` listRecords on
  the subject's own PDS, ordered by `sentAt`)
- `servers` — M19 server-profile reads (`at.atpcraft.server.profile` listRecords
  on the subject's own PDS, preferred `self` rkey, `signingKey` trust
  verification against your own key and `trusted_server_keys`)
- `labels` — M12 labeler read side (AppView view `labels`, labeler
  `#atproto_labeler` endpoint resolution, `com.atproto.label.queryLabels`)
- `labeler` — M59 server-account `app.bsky.labeler.service` declaration at
  rkey `self`: the curated 11-value vocabulary and its client metadata, with
  `createdAt` preservation across republishes
- `triggers` — M61/M62 player-activity label evaluation: the `player`,
  `online`, `admin`, playtime-tier and achievement desired state from captured
  op level and vanilla statistics, the 30-day badge with a daily refresh,
  logout negation, startup recovery and the admin emission/query calls
- `presence` — M13 verified reads of other players' `at.atpcraft.player.snapshot`
  records (`com.atproto.repo.listRecords` on the subject's own PDS, highest
  version, Ed25519 endorsement verification)
- `colibri` — M26 Colibri channel parsing, message/membership record shapes,
  service-auth minting, event decoding, the worker-owned WebSocket loop and
  the M30 local actor-profile check
- `profiles` — M24 Bluesky profile reads (AppView `app.bsky.actor.getProfile`
  plus the direct `app.bsky.actor.profile` record at rkey `self`, with
  atpcraft's direct-record pronoun precedence and whitespace normalisation)
- `skins` — M14 verified reads of other players' `at.atpcraft.player.skin`
  records (newest `timestamp`, endorsement verification) plus
  `com.atproto.sync.getBlob` downloads; `records` carries the shared
  `EndorsementVerdict`/`verify_endorsements` both reads use
- `worker` — owns the tokio runtime, the agent, and the files; the **only**
  module that performs network or disk I/O
- `src/bin/atmospheric-worker.rs` — the M17 sidecar binary: `worker::spawn`
  bridged to stdio as newline-delimited JSON (`Command` in, `Event` out)
- `error` — one error enum, mapped to HUD strings at the edge

### The M2/M17 wiring: how the worker attaches to the game

The injected module lives inside the game's crate, which does not depend on
the `atmospheric` crate, so the two never import each other: they meet at
the channel boundary, and commands cross it as plain data
(`GameCommand`/`ModEvent` mirror `Command`/`Event`).

horizon-mixins 0.6.0 can stage a bundle's own crate into the game workspace
(`[[crate]]`), but its registry dependencies are taken only from Horizon's
existing `Cargo.lock` — the allow list — and the pinned Horizon revision's
lock has no `atrium-api`/`reqwest` (it does have `serde`, `tokio`,
`thiserror`). So the bundle stays mixins-only, and the shipped attach path is
the **M17 sidecar** instead (issue #2 option 3): the game-side module spawns
the `atmospheric-worker` binary next to the game executable and bridges the
channel pair over the child's stdio. When Horizon's lock (or the launcher)
allows it, a small glue helper can alternatively call `install()` and spawn
`atmospheric::worker::spawn` with `native_data_dir("atmospheric")` as the
storage folder; the boundary stays exactly as it is either way.

M2's standalone end-to-end proof still runs the worker against a real PDS —
see "Manual check" below.

### What the worker does with each command (M2)

- `Login` — reads `<storage dir>/config.json` (the only file the mod writes;
  never a world save), calls `session::login`, then
  `com.atproto.server.getSession` to learn the DID, caches agent + DID, and
  emits `Event::Session(LoggedIn { did })` or `Session(Failed { .. })`.
- `SaveConfig` — writes the settings file (creating the storage folder).
- `PostText` — `actions::post_text`, emitting `Event::Posted { uri }` or
  `Event::Failed`; without a session it fails with "not logged in" and never
  touches the network.
- `PostImage` / `RefreshNotifications` — answer `Failed` with the milestone
  that owns them (M3 / M4) instead of panicking the thread.
- `Shutdown`, or the game side dropping the command channel, stops the
  thread cleanly (the blocking channel is bridged into the tokio runtime so
  the loop can `select!` the notification timer, M4, next to the commands).

### M3: screenshot posts

- `actions::post_image` uploads the raw image bytes with
  `com.atproto.repo.uploadBlob`, then writes an `app.bsky.feed.post` whose
  `embed` is `app.bsky.embed.images` (the generated `RecordEmbedRefs` enum
  carries its own `$type`, so only the top-level record needs the injected
  one). PNG posts get an `aspectRatio` read from the IHDR chunk.
- The worker handles `Command::PostImage { path, .. }`: it is the only module
  that reads the image file, rejects files over 2 MB or with a non-image
  magic, and emits `Posted { uri }` / `Failed` as for text.
- Game side: a raw-target HEAD hook on
  `horizon_client::game::chat::Game<'_>::finish_screenshot` (pinned by hash in
  `compatibility.json`) remembers the path of each successfully saved F2
  screenshot in memory — the mod never scans the screenshots folder. The
  second mod keybind (`post_screenshot`, default `key.keyboard.v`) sends
  `GameCommand::PostImage` with that path, or chats "take a screenshot first
  (F2)" when there is none.
- Verified: offline worker tests assert the stub PDS receives the exact image
  bytes on `uploadBlob` and an embed with `$type`/`alt`/`aspectRatio` on
  `createRecord`; in the modded game, F2 saves a screenshot, the keybind after
  it sends the command (no "screenshot first" hint), and before it the hint
  shows; against a real PDS `pds_check` posted an actual in-game screenshot
  (see PR).

### M4: notification polling

- `notifications::poll` calls `app.bsky.notification.listNotifications` with
  the persisted `seenAt`, reduces each item to a `NotificationDigest`
  (uri, author handle, reason, 80-char text snippet), and advances the
  `SeenCursor` past the newest `indexedAt`. The worker persists the cursor to
  `<storage dir>/cursor.json` after any poll that showed something, so
  restarts do not re-show old notifications (the bsky-bot-sdk pattern).
- The worker owns a `tokio::time::Interval` (from `config.poll_interval_secs`,
  minimum 5 s), recreated on each successful login, selected against the
  command channel with `tokio::select!`. Timer ticks poll silently;
  `Command::RefreshNotifications` (sent by `/atmospheric notifications`) polls
  and always reports, emitting `Event::Notifications { items }`.
- Game side: `ModEvent::Notifications` mirrors the digest with plain data, and
  the HUD status line summarizes it ("atmospheric: 2 new: like from alice…").
- Verified: offline worker tests against the stub PDS (which answers
  `listNotifications`) assert the first poll returns both notifications and a
  refresh after it returns none, and that `cursor.json` is persisted at the
  newest timestamp; in the modded game `/atmospheric notifications` shows its
  chat feedback; against a real PDS `pds_check` prints the digest event after
  login.

### M5: atpcraft port start — chat records (`at.atpcraft.player.chat`)

- `base/records.rs` reimplements atpcraft's `at.atpcraft.player.chat` lexicon as a
  plain `ChatRecord { sentAt, message, server }` (message truncated to the
  lexicon's 1024 cap). atpcraft's repo carries no licence, so no code is
  copied — only the wire format is matched, which is the point of a lexicon.
  `base/tid.rs` generates the TID record keys atpcraft uses (53-bit microsecond
  timestamp + 10-bit clock id, sortable base32, 13 chars, strictly monotonic
  per worker), so a player's chat records accumulate as a time-sorted,
  append-only history.
- `actions::post_chat` writes one record per message via `createRecord` with
  the TID as `rkey`; the worker owns the single `TidGen`. `Command::PostChat
  { message, automatic }`: `automatic` (chat-hook origin) publishes only when
  `post_chat_messages` is enabled in `config.json` (default off — chat is
  public once written); `automatic: false` (the `/atmospheric chat <text>`
  command) always publishes. The `server` field is the worker's persisted
  Ed25519 signing key id, matching atpcraft's `signingKey.keyId`; an explicit
  `config.chat_server_id` overrides it.
- Game side: a raw-target injection at HEAD of
  `horizon_client::game::chat::Game::handle_chat_input` (chat.rs is pinned in
  `compatibility.json`; the named hook list has no chat hook) mirrors every
  non-command chat line to the worker. Old settings files without the new
  fields still load (`#[serde(default)]`).
- Verified: `tid` unit tests (alphabet, 13-char width, round-trip, strict
  monotonicity, ordering); `records` tests ($type + camelCase shape,
  truncation); worker tests against the stub PDS assert the createRecord body
  (collection, TID rkey, `$type`, `server`, `message`) and that an automatic
  message with the flag off never touches the network while the explicit
  command still posts; against a real PDS `ATMOSPHERIC_POST_CHAT=… pds_check`
  prints the `Posted` event for a real chat record.

### M6: player-state snapshots (`at.atpcraft.player.snapshot`)

- `base/records.rs` adds the `defs` shapes (`Position`, `Score`, `TeamSummary`,
  `SnapshotSummary`), `Aspect`, `Endorsement`, and `SnapshotRecord`, plus
  `canonical_json` — the deterministic JSON (recursively sorted keys, only
  `\"`/`\\` escaped) that atpcraft signs. `SnapshotRecord::signable_payload`
  builds the exact payload atpcraft's `signablePayload` builds:
  `{did, version, issuedAt, server, summary, aspects[]}` (aspects name-sorted
  as `{name, blob: cid}`).
- `base/signing.rs` is the Ed25519 endorsement key (pure-Rust `ed25519-dalek`,
  BSD-3-Clause; `sha2`/`base64`/`rand_core` MIT OR Apache-2.0 — all
  AGPL-compatible). The key id is the first 16 base64url chars of SHA-256
  over the SPKI encoding (prefix `302a300506032b6570032100` + 32-byte key),
  matching atpcraft's `ServerSigningKey`; signatures are standard base64
  without padding in `sig.$bytes`. The seed is generated once and persisted
  to `<storage>/signing-key.json`; the monotonic version counter persists to
  `<storage>/snapshot-state.json`. The rkey is the key id.
- Worker: `Command::PostSnapshot { summary }` builds the record at
  version+1, signs the canonical payload with the persisted key, and writes
  it via `createRecord`. An empty `summary.username` is filled from the
  configured identifier. Game side: a third keybind (default `C`) and
  `/atmospheric snapshot` build the summary from `native_player` (position +
  dimension; health/food/xp/game-mode default until Horizon exposes them).
- Verified: RFC 8032 seed→public-key vector cross-checked against openssl;
  key-id/sign/verify round-trips; canonical-JSON ordering test; snapshot
  record wire shape ($type, camelCase, `sig.$bytes`); worker test against the
  stub PDS asserts the createRecord body (collection, key-id rkey, version 1
  then 2, username default) and that the signature verifies against the
  persisted public key over the payload rebuilt from the record; against a
  real PDS `ATMOSPHERIC_POST_SNAPSHOT=1 pds_check` posts a real snapshot.

### Issue sweep (session resume, death messages, HUD history)

- **#17 HUD lines expire**: the HUD status box keeps at most
  `STATUS_LINES_MAX` lines and each line now carries a timestamp; lines older
  than `STATUS_LINE_TTL` (30 s) are dropped on every drain/read, so stale
  statuses do not linger forever.

- **#5 session persistence**: `session::login` now returns the `StoredSession`
  (pds_url/did/handle/refresh_jwt — never the app password), persisted by the
  worker to `<storage>/session.json` when `persist_session` is on (default).
  **#16 file permissions**: every file that holds a secret — `session.json`
  (refresh token; for OAuth also the DPoP private JWK), `signing-key.json`
  (Ed25519 seed) and `config.json` (app password) — is written through
  `worker::write_private`, which sets mode 0600 on unix (no portable ACL API
  elsewhere; see the README).
  At startup the worker calls `session::resume`: a direct
  `com.atproto.server.refreshSession` with the stored refresh token (the
  agent cannot seed its store without a full session, so that one call goes
  through reqwest), then `resume_session` + `getSession`. The rotated refresh
  token is persisted again. **#18**: when a persisted session cannot be
  resumed (expired/revoked token, PDS unreachable) the worker now emits
  `Session(Failed { .. })` instead of staying silent; the persisted file is
  kept so a later start retries. `Command::Logout` (`/atmospheric logout`) drops
  the session and deletes the file. Stub-PDS tests cover resume-without-login,
  token rotation, and logout.
- **#6 death messages**: a `RETURN` injection on
  `horizon_client::game::chat::Game::death_message` reads `ui.death_text`
  (set just before return) and hands `Text::plain()` to the worker, which
  posts it as an `app.bsky.feed.post` only when `post_death_messages` is
  enabled. Tested both ways against the stub PDS.
- **#7 HUD history**: `runtime` keeps the last `STATUS_LINES_MAX` (3) status
  lines instead of only the newest; the HUD box draws them stacked. Tested.

### M8: localhost OAuth login (DPoP-bound, replacing app passwords)

`/atmospheric login <handle>` sends `Command::LoginOAuth`; bare
`/atmospheric login` keeps the app-password flow as a fallback. The flow
lives in `crates/atmospheric/src/account/oauth/mod.rs` and follows the atproto OAuth
profile (atproto.com/specs/oauth), reimplemented from the spec:

1. resolve handle → DID (`com.atproto.identity.resolveHandle` on bsky.social)
   → DID document (plc.directory / did:web) → PDS endpoint → PDS
   `/.well-known/oauth-protected-resource` → authorization server metadata;
2. bind a loopback listener on `http://127.0.0.1:<port>/` and push the
   authorization request (PAR) with PKCE S256, the DPoP key thumbprint
   (`dpop_jkt`) and `login_hint`, retrying with the server's `DPoP-Nonce`;
3. emit `Event::OAuthAuthorize { url }` (HUD/chat shows it) *before* trying
   to open the system browser at the authorize URL; if no browser can be
   launched, emit `Event::Failed` with the manual link (the same fallback
   Sequoia's CLI uses) — then wait for the redirect on the loopback
   listener, checking `state`. Tests and headless runs pass
   `WorkerOptions::launch_browser = false` so `cargo test` never spawns a
   browser (`pds_check`: `ATMOSPHERIC_NO_BROWSER=1`);
4. exchange the code at the token endpoint with a DPoP proof; validate
   `sub` == the resolved DID, `token_type` == `DPoP`, and that the granted
   scope contains `atproto`.

The `client_id` is the spec's loopback form `http://localhost?scope=…`
(verified against the real bsky.social and eurosky.social authorization
servers: PAR accepted, `request_uri` issued). Authorization servers without
the loopback exception need a hosted client-metadata URL instead — this
repo's `oauth-client-metadata.json` is served via GitHub Pages at
`https://ewanc26.github.io/atmospheric-horizon/oauth-client-metadata.json`
(#21); set `oauth_client_id` in `config.json` to that URL.

DPoP binding: a per-install P-256 key (ES256 proofs, persisted as a private
JWK in `session.json`) signs every request. `account/session.rs` has a custom
`SessionStore` whose `authorization_token` answers `AuthorizationToken::Dpop`
while DPoP state is installed (Bearer otherwise), and a `DpopClient`
wrapping the reqwest XRPC client that adds the `DPoP` proof header
(htm/htu/ath, nonce retry) to every request. One agent type therefore serves
both login flows. The worker refreshes the access token proactively at the
token endpoint when near expiry (the agent's own ExpiredToken auto-refresh
only knows the app-password `refreshSession` endpoint); at startup a
persisted OAuth session resumes through the refresh grant. The browser flow
runs on a spawned task with its result on a channel, so `Shutdown` stays
responsive while the browser is open.

Offline tests (`oauth/tests.rs` + worker tests): a stub AS/PDS verifies every
DPoP proof signature, the nonce retry, `dpop_jkt`, PKCE, the state check, the
full login → DPoP-signed `createRecord` path, startup resume via refresh,
and shutdown-during-pending-login. Real-PDS wire check: `pds_check` with
`ATMOSPHERIC_OAUTH_PAR_ONLY=1` (see below) pushed a PAR accepted by
eurosky.social and bsky.social.

### M9: player skin records (`at.atpcraft.player.skin`)

`/atmospheric skin [path] [default|slim]` sends `Command::PostSkin`; with no
path the worker falls back to `skin_path`/`cape_path` in `config.json` (the
worker is the only module that touches disk). The flow, matching atpcraft's
`PlayerSkinWriter`/`PlayerSkinRecord` wire format (reimplemented as data from
its lexicons, no code copied):

1. read the PNG(s), reject anything that is not `image/png` or over the
   lexicon's 1 MiB blob cap before any network call;
2. `com.atproto.repo.uploadBlob` each PNG (`$type: "blob"` refs come back);
3. `com.atproto.repo.getRecord` at rkey = the mod's Ed25519 key id and keep
   endorsements from *other* keys (atpcraft's `create(existing)` semantics);
4. build the record `{skin?, cape?, model, timestamp, endorsements}` and sign
   the canonical payload `{did, skinCid?, capeCid?, model, timestamp, server}`
   (key-sorted JSON, same `canonical_json` as M6; asset keys omitted when
   absent) — signature is base64-no-pad Ed25519, alg `ed25519`;
5. `com.atproto.repo.putRecord` (upsert, `validate: false`) at the key id —
   not `createRecord`, so re-publishing replaces rather than fails.

Verified against the real PDS: `pds_check` with `ATMOSPHERIC_POST_SKIN=…`
logged in as the test account and upserted
`at://…/at.atpcraft.player.skin/<key id>` on eurosky.social; the stored record
was read back and matches the lexicon exactly (test record deleted after).
Stub-PDS tests cover the upload+upsert path, the endorsement verifying against
the persisted key, foreign-endorsement preservation across re-publish, and
the non-PNG/no-path rejections (no upload happens).

### M10: server profile record (`at.atpcraft.server.profile`)

`/atmospheric server` sends `Command::PostServerProfile`; the worker builds
the descriptor from the settings file (identity fields, capabilities, game
version; a blank `server_name` falls back to `client_name`, and an empty
capabilities list is derived from active features), the logged-in DID/handle
as `operator`, and the mod's persisted
Ed25519 key as the `signingKey` block (`publicKey` = base64-standard SPKI).
It is upserted with `putRecord` (validate=false) at the literal rkey `self`;
a prior `getRecord` preserves `createdAt` across re-publishes (first publish
stamps it), matching atpcraft's `ServerProfileWriter`. Capabilities are
sorted/deduped as the lexicon requires; blank optionals are omitted. The
record carries no signature — it lives in the account's own repo.

Verified against the real PDS: `pds_check` with
`ATMOSPHERIC_POST_SERVER_PROFILE=1` upserted
`at://did:plc:…/at.atpcraft.server.profile/self` on eurosky.social and the
stored record read back matched the lexicon exactly (test record deleted
after). Stub-PDS tests cover the shape, key-id/public-key match against the
persisted key, capability sorting, and `createdAt` preservation.

### M11: join-gate list reads (`app.bsky.graph.getList`)

`/atmospheric list <reference>` sends `Command::FetchList`; the worker parses
the reference (an `at://<did>/app.bsky.graph.list/<rkey>` URI, or a
`https://bsky.app/profile/<owner>/lists/<rkey>` URL — a handle owner is
resolved to a DID via the same `resolveHandle` service OAuth login uses),
then pages `app.bsky.graph.getList` (`limit=100`, cursor until absent, capped
at 200 pages) against the AppView in `config.json` (`appview_url`, default
`https://public.api.bsky.app` — the public, unauthenticated AppView atpcraft
uses) and emits `Event::ListMembers { list, members }` with
`items[].subject.did`. Works logged out. This is the read side of atpcraft's
join gate (`AtprotoListResolver`), reimplemented from the lexicon semantics
only.

Verified against the real public AppView: `pds_check` with
`ATMOSPHERIC_FETCH_LIST` fetched 9 members of the test account's "My
Supporters" list (matching its `listItemCount`) and 21 members via a
`bsky.app/profile/bsky.app/lists/…` URL (handle resolved to the owner DID).
Stub tests cover reference parsing, two-page pagination, and the error path.

### M12: labeler read side (`com.atproto.label.*`)

Issue #29 kept only the client-side halves of the labeler item: (a) reading
labels on a subject, and (b) resolving a labeler's DID-document service
endpoint. Label issuance stays server-side; M56–M59 now provide the signing,
durable log, `queryLabels` and `subscribeLabels` layer described above (a client
mod still cannot host the labeler HTTP endpoint).

- `social/labels.rs` parses a subject reference — an `at://` URI (account or
  record), a bare DID, a handle, or a bsky.app profile/post/list URL — and
  resolves handles to DIDs via the same `resolveHandle` service OAuth login
  uses. Account labels carry the bare DID as their `uri`; record labels the
  full AT-URI, so `ResolvedSubject` exposes both forms.
- `Command::FetchLabels { subject }` (`/atmospheric labels <subject>`) reads
  the `labels` array off the subject's AppView view: `app.bsky.actor.getProfile`
  for accounts, `app.bsky.feed.getPosts` for posts, `app.bsky.graph.getList`
  (first page, limit 1) for lists. Unauthenticated against the configured
  AppView (`appview_url`, default `https://public.api.bsky.app`), so it works
  logged out; other collections have no AppView view with labels and get a
  readable error pointing at the labeler command.
- `Command::FetchLabelerLabels { labeler, subject }`
  (`/atmospheric labeler <did> [subject]`) resolves the labeler DID's
  document (plc.directory / did:web, as in M8), extracts the
  `#atproto_labeler` (`AtprotoLabeler`) service endpoint — the same service
  extraction shape as `#atproto_pds` — and pages
  `com.atproto.label.queryLabels` (`uriPatterns`, limit 100, cursor until
  absent, capped at 100 pages) at that endpoint. `subject` defaults to the
  logged-in account's own DID (the atpcraft case: a server labeler labeling
  player accounts); logged out with no subject it fails fast as
  "not logged in" before any labeler HTTP call.
- Both emit `Event::Labels { subject, via, labels }` with
  `labels::LabelDigest { val, src, uri, neg, cts, exp }` reduced from
  `com.atproto.label.defs#label`; the game side mirrors it as plain data and
  the HUD summarizes it ("atmospheric: 2 label(s) on did:plc:… via …:
  player, -spam"). Wire shapes come from the lexicons/spec only — no code is
  copied from atpcraft (its repo carries no licence); atpcraft's labeler
  answers the same spec endpoints.
- Verified: `labels` unit tests (subject parsing, `#atproto_labeler`
  extraction, view routing per collection, two-page `queryLabels` against a
  stub) and worker tests against the stub (which doubles as AppView, PLC
  directory and labeler): post/handle/list subjects, the labeler query with
  DID-doc resolution, the no-labeler-service failure, and the bad-subject /
  logged-out paths making no HTTP calls. Real-AppView wire check: `pds_check`
  with `ATMOSPHERIC_FETCH_LABELS` / `ATMOSPHERIC_LABELER` (optional
  `ATMOSPHERIC_LABELER_SUBJECT`).

### M13: player presence reads (`at.atpcraft.player.snapshot`)

`/atmospheric player <did|handle>` sends `Command::FetchPlayer` — the read
side of atpcraft's player-state sync, so a client can see where a player
last was, with the record's server endorsement checked. The flow, in
`social/presence.rs`:

1. `resolve_player` parses the subject (reusing the M12 subject parser;
   record URIs are rejected — a player is an account), resolves handles via
   the same `resolveHandle` service OAuth login uses, fetches the DID
   document (plc.directory / did:web), and takes its `#atproto_pds` endpoint;
2. `list_records` pages `com.atproto.repo.listRecords`
   (`collection=at.atpcraft.player.snapshot`, `limit=100`, cursor until
   absent, capped at 100 pages) against that PDS, **unauthenticated** —
   public repos serve it, so the read works logged out;
3. `latest` keeps the structurally valid record with the highest `version`
   (aspect names sorted and unique, as atpcraft's `fromJson` enforces);
4. `verify` mirrors atpcraft's `PlayerSnapshotRecord.verify`: for each
   `ed25519` endorsement, the canonical payload is rebuilt from the raw
   record JSON (`snapshot_payload` — `{did, version, issuedAt, server,
   summary, aspects}` with aspects name-sorted as `{name, blob: cid}` or
   `{name, inline: base64url}`, so foreign records with inline aspects
   verify too; a unit test asserts parity with the M6 writer's
   `signable_payload` for blob aspects) and checked against a key-id →
   public-key map. The map is the mod's own persisted signing key (loaded
   only if present — a read never creates one) plus `trusted_server_keys`
   from `config.json` (`keyId` + base64 SPKI `publicKey`, key id derived
   when blank, malformed entries skipped — atpcraft's `TrustedServerKeys`
   semantics). Verdicts: `Verified { endorser }` / `UnknownEndorser` /
   `BadSignature` / `Malformed` (the server-side rollback check is not a
   client concern).

The worker emits `Event::PlayerSnapshot { subject, via, snapshot }` with a
`PlayerDigest` (username, dimension, position, version, issuedAt, verdict);
the game side mirrors it as plain data (boxed — it is the event enum's
largest variant) and the HUD summarizes it ("atmospheric: Steve v7 in
minecraft:overworld at -25, 67, -3 — verified by keyid…"). Wire shapes come
from the lexicons only — no code is copied from atpcraft (its repo carries no
licence).

Verified: `presence` unit tests (two-page `listRecords` against a stub,
payload parity with the M6 writer, inline-aspect payloads, all four
verdicts, unsorted-aspect rejection, latest-version selection, digest
fields) and worker tests against the stub PDS (which now stores
`createRecord` bodies and serves `listRecords` per repo): a posted snapshot
reads back verified by the persisted key, a foreign key trusted via config
verifies, a tampered record reports `BadSignature`, an untrusted endorser
reports `UnknownEndorser`, a record-URI subject fails before any HTTP call,
a repo with no records reports `None`, and the `listRecords` request is
asserted unauthenticated. Real-PDS wire check: `pds_check` with
`ATMOSPHERIC_FETCH_PLAYER` (and `ATMOSPHERIC_TRUSTED_SERVER_KEYS` for
trusted-key verification).

### M24: profile reads (`app.bsky.actor.getProfile` + `app.bsky.actor.profile`)

`/atmospheric profile <did|handle>` sends `Command::FetchProfile` — the
client-side half of atpcraft's profile pronouns, with the same fallback rule
atpcraft uses during OAuth. The flow, in `social/profiles.rs`:

1. `resolve_profile` parses the subject with the M12 subject parser (record
   URIs are rejected — a profile is an account), resolves handles through the
   same directory service OAuth login uses, fetches the DID document and
   takes its `#atproto_pds` endpoint.
2. The AppView's `app.bsky.actor.getProfile` (`actor` = the DID) and the
   subject PDS's `com.atproto.repo.getRecord` (`collection`
   `app.bsky.actor.profile`, `rkey` `self`) are both read
   **unauthenticated**, so the command works logged out.
3. `digest` combines them. A successful direct-record read owns `pronouns`,
   including the absence of the field — this clears an older AppView value.
   Only a failed record read falls back to the AppView's pronouns. Display
   names use the AppView value when non-empty and fall back to the direct
   record; the description does the same. Pronouns are trimmed and internal
   whitespace is collapsed; a blank value is no value.

The worker emits `Event::Profile { subject, via, profile }` with a
`ProfileDigest` (DID, handle, display name, description, pronouns, avatar URL).
The game side mirrors it as plain data and the HUD shows the display name,
`(@handle)`, pronouns in parentheses and a truncated description. Nothing is
stored on disk. The direct profile record is read from the PDS; its avatar is
a blob reference, so only the AppView's avatar URL is carried in the digest.

atpcraft's remaining server-side surfaces (RPG item discovery and full aspect
capture/restore) are not implemented yet; the README port table records each
one. M56–M62 cover label signing, durable storage, querying, streaming, the
server-account declaration, its PLC identity publication and the game triggers
for the full automatic vocabulary, including op-level and vanilla-statistic
labels. M25 supplies the separate server account, M26/M27 run the operational
Colibri bridge, M35 supplies the shared player-to-DID map, M36 claims links from
the game, M37 checks linked players on join and M38 enforces the configured
list; capture remains a separate milestone.

Verified: `profiles` unit tests (whitespace normalisation, direct-record
precedence, clearing on a successful record without pronouns, AppView
fallback, direct-only profiles, both-sources-failed) and a worker test against
the stub (AppView fallback, direct-record pronouns, clearing, both reads
unauthenticated, and a bad subject making no HTTP call). The runtime command,
sidecar wire shape and HUD line are covered by the std-only encode/decode and
status tests.

### M25: the separate server account

atpcraft acts as a long-lived server account for its labeler, Colibri service
auth, RPG attestations and gate; the player session is not that identity.
`/atmospheric server-account login <handle>` therefore starts a second
localhost OAuth flow (`Command::LoginServerOAuth`), tagged with
`OAuthTarget::Server` so its result cannot overwrite the player session. The
worker keeps `server_agent`, `server_did`, `server_handle`, its own
`server_dpop`, and a `server_stored_session`; the last is written to
`server-session.json` with the same owner-only protection as `session.json`.

At startup `try_resume_server` refreshes the server session independently;
`/atmospheric server-account status` emits `Event::ServerAccount { state,
handle, did }`; `/atmospheric server-account logout` drops the agent and
removes the file. `ensure_server_oauth_fresh` refreshes its DPoP-bound token
before a server-profile write. `/atmospheric server-account publish`
(`Command::PostServerProfileAsServer`) writes `at.atpcraft.server.profile` at
rkey `self` from the server account, while the existing `/atmospheric server`
keeps writing from the player account. Both use the same shared profile writer
and the mod's persisted Ed25519 endorsement key; `createdAt` is preserved.

The session file contains a DPoP private key and a refresh token, so it is a
secret exactly like `session.json`; the README's Network use section records
it. The account is the foundation, not the whole server-side feature: the
aspect sync still needs its own milestone; M56–M62 add the labeler service,
declaration, PLC identity, game triggers and op-level/statistic labels, and M38
supplies linked-account join enforcement. M26 builds the first Colibri surface on this
account.

Verified: a worker test runs the full stub OAuth flow for the server account,
asserts the `ServerAccount` LoggingIn/LoggedIn/LoggedOut events, the handle
and DID, the separate `server-session.json` (0600 on unix, no password), the
absence of a player `session.json`, and file deletion on logout. The runtime
and sidecar wire shapes for the new commands/events are pinned by the
std-only encode/decode tests.

### M26: Colibri chat bridge core

Colibri is atpcraft's chat layer: a channel lives in a community repo and the
AppView streams message upserts to members. The bridge needs two identities —
the server account mints the stream token, while each chat line is written by
the player who sent it. M26 implements that core without inventing a server
thread in the client:

1. `ChannelRef::parse` accepts the channel AT-URI, a `https://…/app/c/…` link,
or the `social.colibri:/channel/…` deep link. It derives the channel URI, the
   community DID/rkey, and rejects a wrong collection, channel type or rkey.
2. `/atmospheric colibri start` loads `colibri_channel_uri` and
   `colibri_appview_url`, requires the M25 server account, refreshes its DPoP
   token, and calls `com.atproto.server.getServiceAuth` with
   `aud=<colibri_appview_did>#colibri_appview` and
   `lxm=social.colibri.sync.subscribeEvents`. The OAuth scope adds the three
   Colibri grants whenever `colibri_bridge_enabled` is true, so a session
   created before the bridge was enabled must be authorised again.
3. The worker spawns `colibri::run_stream`. It opens the WebSocket with the
   `colibri.auth.bearer` subprotocol, sends a heartbeat every 30 seconds,
   decodes `message_event` upserts, reconnects with 1–60 second backoff, and
   reports `Connected`/`Disconnected`/`Message` updates on an mpsc channel.
4. The worker filters other channels and its own recently written AT-URIs,
   then emits `Event::ColibriMessage`. The runtime queues the sanitised line
   and the tick hook sends it to native chat; the same event also feeds the
   HUD.
5. `Command::ColibriSend` writes `social.colibri.message` with `text`,
   `channel` and `createdAt` as the player account. The ordinary chat hook
   sends the same command automatically, so chat is mirrored only while the
   bridge is active; an explicit `/atmospheric colibri send` reports errors
   while the automatic path stays silent.

`colibri_bridge_enabled` arms the bridge automatically after a server login or
session resume. `/atmospheric colibri stop` aborts the socket task and drops
the channel; `/atmospheric colibri status` reports active/connected state.

Not in M26: join/leave/death/advancement notices, approval and profile
prompts. M27 adds the server account's community membership and proactive
45-minute token rotation. The service token is in memory only; no new
credential is written to disk.

Verified: `colibri` unit tests cover all three channel-link forms, the old
community-rkey form, record shapes and text caps, audience/stream URL/
subprotocol construction, event decoding, and a loopback WebSocket server that
asserts `Connected` plus a delivered message. Worker tests cover start without
a server account and an explicit send while stopped. The runtime tests pin the
four command wire shapes, both event shapes, HUD text, the automatic chat fan
out, and native-chat draining. No live Colibri AppView was contacted.

### M27: Colibri membership and token rotation

The AppView only streams a community's events to accounts that carry a
`social.colibri.membership` record, and `getServiceAuth` tokens live at most
one hour. `start_colibri` now repairs the first and the stream task handles the
second:

1. Before minting the stream token, the worker pages the server account's
   `social.colibri.membership` records with an authenticated
   `com.atproto.repo.listRecords` (limit 100). If none carries the channel's
   community URI, it writes one with `com.atproto.repo.createRecord` at a fresh
   TID. It deliberately does not use `putRecord`: the Colibri OAuth grant
   carries create/delete, not update. A second start reuses the record instead
   of creating a duplicate.
2. `connect_once` arms a 45-minute rotation timer. When it fires, the socket
   reports `StreamEvent::Rotate` and returns; the worker's async update handler
   calls `start_colibri` again. That path re-runs membership repair (a no-op),
   refreshes the server account's DPoP token if needed, mints a fresh
   service-auth token, and spawns a new stream. Network drops still reconnect
   inside the same stream task with 1–60 second backoff.

The membership record is public and owned by the server account; its
`createdAt` is the write time. No membership state or token is persisted
locally. Approval-gated communities still accept the membership record as
pending, but the bridge does not yet tell the operator to wait for a moderator.

Verified: a worker test logs an agent into the stub PDS, runs
`ensure_membership` twice, and asserts one `createRecord` with the expected
`community` and no duplicate. The full gate covers the rotation code's types;
time itself is not accelerated in a test, so no test waits 45 minutes. No live
Colibri AppView was contacted.

### M28: Colibri server event notices

Horizon's server-side `broadcast_message` hook (`hook_version = 1`, the
`broadcast_message` name from `native-hooks.json`) runs before a server system
line is logged and sent. `mixins/atmospheric/colibri/notice.rs` takes the
line's `Text`, reads its `translate` key and asks the std-only runtime
`is_colibri_notice` classifier. That accepts exactly atpcraft's categories:
`multiplayer.player.joined`, `multiplayer.player.joined.renamed`,
`multiplayer.player.left`,
`chat.type.advancement.task|goal|challenge`, and any `death.*` key. Ordinary
`chat.type.text`, command output and unknown keys are dropped.

The runtime then sends `GameCommand::ColibriNotice { text }`. The worker
loads `colibri_server_events_enabled` (default true), strips control and
legacy colour characters, caps the text at the lexicon limit, and writes the
localised line as `social.colibri.message` under the **server account**. It
uses the same echo guard as player sends. The path is deliberately quiet: a
missing bridge, a disabled setting or an automatic write failure never
interrupts the game.

This observes vanilla broadcast lines only. M29 adds the fallback for lines
that another mod rewrites or hides, and the pending-approval notice. The hook
still does not cover a client connected to a remote server that does not run
the mod; M37 later adds the server-wide Colibri profile prompt.

Verified: the runtime classifier test covers all accepted and rejected keys;
the runtime forwarding test asserts the `ColibriNotice` command and the codec
pins its wire shape. The worker's write path reuses the already-tested
`colibri::message_record` and `create_record_json`; no live server or AppView
was contacted.

### M29: Colibri presence fallback and approval notice

The connection hooks `player_logged_in` and `player_logged_out`
(`hook_version = 1`, server side) call `colibri/join.rs` and
`colibri/leave.rs`. Each localises the join/leave line through
`Text::translate`, then sends `GameCommand::ColibriPresence { kind, text }`.
The join helper also sends M37's `GameCommand::ColibriPlayerJoined` with the
player's UUID. The worker waits **3 seconds** (`COLIBRI_PRESENCE_WAIT`) and asks whether a
matching line arrived within **5 seconds** before or after the event
(`COLIBRI_PRESENCE_WINDOW`), the same window atpcraft uses.

`colibri/notice.rs` marks handled lines through `ColibriHandled { kind }`.
Join and leave lines mark their kind; a textless line marks `None`, which
counts for both kinds. This matters when a chat mod intentionally hides a
line: the hidden line must suppress the fallback, not trigger a duplicate.
The worker keeps the last time for join, leave and blank lines, and a
scheduled fallback is dropped when its generation predates the current bridge
(`colibri_started_at`), so a stop/start cannot write a stale notice. The
fallback is off when either `colibri_server_events_enabled` or the new
`colibri_server_events_fallback_enabled` switch is false.

After `ensure_membership` creates a record, the worker calls
`colibri::requires_approval`: resolve the community DID document with the
shared OAuth resolver, take its `#atproto_pds` endpoint, and issue an
unauthenticated `com.atproto.repo.getRecord` for
`social.colibri.community`/`self`. `requiresApprovalToJoin == true` emits
`Event::Status`, which the runtime renders as a HUD line. A failed read or a
missing flag is silent; an existing membership is never re-checked.

Verified: the worker test seeds the community record in the stub PDS and
asserts the `getRecord` read returns `Some(true)`; the pure approval parser
covers true/false/missing values, and the fallback window test covers the
inclusive 5-second boundary, blank-line suppression, the other kind and
unknown kinds. The injected connection hooks themselves are not compiled by
`cargo test`; no live server or Colibri AppView was contacted.

### M30: Colibri local profile prompt

atpcraft checks each linked player's `social.colibri.actor.profile` on join and
sends a two-line setup prompt when the `self` record is absent. This client mod
has no server-wide player-to-ATProto mapping, so M30 checks the only account it
knows: the local player's session.

`start_colibri` calls `check_colibri_profile` after the bridge is armed, and a
player login calls it when a bridge is already active. It refreshes the player
OAuth token if needed, then reads `social.colibri.actor.profile`/`self` with
`actions::get_record` as that player. A missing record emits
`Event::ColibriProfileMissing { url }` once per bridge session; an existing
record and any failed read stay quiet, matching atpcraft's `EXISTS` and
`UNAVAILABLE`. The runtime queues atpcraft's two chat lines ("You need a
Colibri profile…" and "Open Colibri: https://colibri.social") and folds the
same missing-profile message into the HUD.

The check resets on an explicit bridge stop and on player logout, so a new
session or account is checked again. A 45-minute token rotation does not repeat
the prompt. M37 extends the same check to other linked players when they join.

Verified: a worker test logs an agent into the stub PDS, points the worker at a
channel, and calls the check with no profile (asserting the
`ColibriProfileMissing` event and the `getRecord` request), then seeds the
`self` record and asserts silence. Runtime tests pin the event wire shape, HUD
line, and both chat lines. No live Colibri AppView was contacted.

### M31: Colibri reconnect and send throttle

atpcraft exposes an operator `/atpcraft colibri reconnect` and caps each
player's Colibri writes at 20 per rolling minute. Both are client-observable
for the local player, so M31 ports them without the server-wide identity map.

`Command::ColibriReconnect` requires an active bridge and a logged-in server
account. It re-reads the appview URL and audience from `config.json`, refreshes
the server account's OAuth token if needed, and mints a fresh service-auth
token with `com.atproto.server.getServiceAuth`. `spawn_colibri_stream` aborts
the old socket task and installs a new one. Unlike `start_colibri`, reconnect
does not list or create membership, does not re-check the local actor profile,
and preserves `colibri_sent`, `colibri_started_at`, the presence timestamps
and `colibri_profile_checked`; a fallback already in flight therefore remains
valid. `/atmospheric colibri reconnect` follows atpcraft's rule that the bridge
must be on; the runtime shows the worker's failure when it is not.

The send limit is a `VecDeque<Instant>` of accepted player writes. Each call
first drops timestamps at least 60 seconds old, then refuses the write when
20 remain, matching atpcraft's inclusive 60-second boundary. The queue is tied
to the local DID and cleared when the account changes. The check runs before
token refresh or the PDS write, so a throttled message costs no network call.
Server notices call `send_colibri_notice` directly and are exempt; automatic
chat-hook messages stay silent at the limit, while the explicit
`/atmospheric colibri send` reports it.

Verified: unit tests cover the 20th/21st boundary, the exact 60-second expiry,
and the explicit failure; worker tests cover reconnect without a bridge and a
live reconnect against a loopback WebSocket that asserts a fresh
`getServiceAuth` token, the subprotocol value, `Connected`, and no membership
request. Runtime tests pin the new command's wire shape and forwarding. No
live Colibri AppView was contacted.

### M32: local Colibri player membership

atpcraft writes `social.colibri.membership` in each authorized player's own
repo so the AppView can index that player's messages. The server account's
record is what lets the bridge receive community events; the player's record
is a separate membership in the player's repo. M27 handled only the first, so
M32 adds the local player's own record without needing the server-wide
player-to-ATProto map.

`check_colibri_player_membership` runs after the bridge is armed and after a
player login while it is active. It refreshes the player token if needed, calls
the existing `colibri::ensure_membership` with the player's agent and DID, and
marks the guard only after the list/create succeeds. A failed list or write
stays silent and leaves the guard unset, so a later login or token rotation can
retry; the bridge itself never fails because of the player-side write. Existing
records are reused and no duplicate is created.

When `ensure_membership` returns created, the worker reads the community's
`requiresApprovalToJoin` through the same unauthenticated
`colibri::requires_approval` path as M29. A true value emits
`Event::ColibriMembershipPending { message }`; the runtime adds atpcraft's
`[Colibri] Your join is pending. A moderator must approve it.` line to native
chat and the HUD. An existing membership, a false/missing flag, or a failed
read emits nothing. The guard resets on explicit bridge stop and on player
logout/login, but not on the 45-minute token rotation or a manual reconnect.

Verified: a worker test starts with an active bridge, logs the player in against
the stub PDS, seeds an approval-gated community, and asserts one membership
`createRecord`, the pending event, and no duplicate on a forced second check.
Runtime tests pin the event wire shape, HUD text, and native-chat line. The
injected game hooks themselves are not compiled by `cargo test`; no live
Colibri AppView was contacted.

### M33: Colibri incoming chat hygiene

Incoming Colibri messages are remote input. atpcraft's
`ColibriChatFormatter.clean` replaces control characters and the legacy
section sign with spaces, collapses every whitespace run, trims, and caps the
handle at 64 characters and the text at 512. The runtime previously removed
unsafe characters instead, so a newline could join two words, and it used the
512 cap for handles as well.

`clean_remote(value, max)` now follows atpcraft's order: unsafe or whitespace
characters mark a pending single space (ignored before the first visible
character), the next visible character emits it, and the final string is capped
only after trimming. The profile setup URL and membership-pending line use the
text cap; incoming handles use 64 and message text uses 512. The line still
starts with `[Colibri]`, as the native-chat bridge does not use atpcraft's
legacy colour prefix.

Verified: a runtime test covers newlines/tabs, NUL and section signs, repeated
spaces, the 64-character handle cap, and the formatted chat line. Existing
profile, membership and message tests remain unchanged. No live Colibri
AppView was contacted.

### M34: Colibri status detail

atpcraft's admin status prints the channel URI and the owning community URI
separately. `Event::Colibri` previously carried only the channel, so the port
could not show which community the bridge belonged to. The worker now fills
`community` from `ChannelRef::community_uri()` (the channel's authority plus
`social.colibri.community/self`). The runtime decoder requires the field and the
connected/connecting/disconnected status line reads `... to <channel> in
<community>` (or `from ...` when an error is present). Bridge-off output is
unchanged.

Verified: the wire decoder test pins the new field, the status test checks the
combined line, and the reconnect test asserts the parsed community URI. No new
network request is made and no live Colibri AppView was contacted.

### M35: server-side player identity service

M30-M34 can only inspect the local player because the client has no shared
mapping from a Minecraft UUID to an ATProto DID. M35 adds that map as an
optional separate process, `crates/atmospheric-server`, rather than pretending
the game client can know other players' accounts.

The service exposes `GET /healthz` and public `GET /v1/links/{uuid}` reads, an
authenticated `GET /v1/links` listing, authenticated `PUT`/`DELETE` on
`/v1/links/{uuid}`, authenticated `POST /v1/pairings`, public
`POST /v1/pairings/claim`, and the M39 browser routes (authenticated
`POST /v1/browser/pairings`, public client metadata and link pages). M64 adds
the authenticated `POST /v1/browser/skins` creation route plus the public
`/skin/{token}` pages and `/skin/callback`. Admin routes require
`Authorization: Bearer $ATMOSPHERIC_SERVER_TOKEN`; if the variable is unset
they return 503 instead of allowing an unauthenticated write. Pairing codes are
random, one-time, default to ten minutes, accept 60-3600 seconds and are stored
canonicalised without their display hyphen. Creating a new code for a UUID
invalidates that UUID's previous code. The experimental #189 operator routes
are additive to this same process; see "Operator's Warrant (#189)" above for
the process-boundary rules.

The store is a single JSON document (`links.json`) protected by an async write
lock. Every mutation clones the state, writes a temporary file with owner-only
permissions on Unix, fsyncs it, renames it over the old file (atomic on the
Unix/Docker target), and only then commits the new state in memory. A failed
write therefore leaves both copies unchanged. UUIDs, DIDs, handles, player names, codes and TTLs are normalised and
validated before they reach the store. The service is designed for one process
per data directory, not for multiple replicas sharing a volume.

M36 wires the explicit `/atmospheric link <code>` claim, M37 wires the
public join-time link lookup, M38 consumes the same map for login-gate
enforcement, and M39 adds browser OAuth link creation and callback routes.
The service is still the foundation for RPG/player-data work; those consumers
remain separate and must not be described as delivered. Label emission is wired
(M61/M62), and the game now supplies op level and vanilla statistics on every
sweep, from which the worker derives the status and milestone labels without
storing the captured values.

Verified: `model` unit tests cover UUID, name, DID, handle, code and TTL
validation; store tests cover reopen persistence, `linked_at` preservation,
one-time and expired pairings, and idempotent deletion; router tests cover
public reads, admin authentication, direct writes, deletion and a claim that
survives a store reopen, plus an M40 full browser OAuth flow against a local
stub that verifies PKCE and DPoP before the link is persisted. `docker/server-test.sh` builds the image, runs the
container against a named volume, exercises the API including an invalid DID,
restarts the container and checks links still resolve. No network service
outside the container is contacted.

### M36: claiming a server identity link in game

M35 left the service without a client path. M36 adds `server_api_url` to
`config.json` (empty by default) and `/atmospheric link <code>`. The command
sends `Command::ClaimServerLink`; the worker requires a logged-in player, loads
the settings, and calls `identity::claim`. The request is a JSON
`POST <server_api_url>/v1/pairings/claim` with `{code, did, handle?}`, using the
stored player handle when one exists. It carries no admin token, access token,
app password or game state.

A successful claim returns the UUID, player name, DID and handle the service
bound. The worker rejects a response naming a different DID or missing required
fields, then emits a plain `Event::Status` line with the player name, UUID and
DID. A service error is parsed from `{"error": "..."}` and shown through
`Event::Failed`; an empty `server_api_url`, a missing login and an unreadable
settings file fail before any request. The runtime encodes the new command and
adds it to the usage text. No new server route was needed: the M35 endpoint is
the contract.

Verified: four `identity` tests cover the exact path/body, optional-handle
omission, service-error propagation, DID mismatch rejection and non-HTTP base
URLs; a worker test logs in against the stub PDS, claims a code against the
same stub's identity route, and asserts the DID, handle and status line; the
sidecar test covers a claim while logged out; the runtime test pins the command
wire shape.

### M37: server-wide Colibri profile checks

M30 could only inspect the local player because it had no UUID-to-DID map.
M37 uses the map M35 built and the claim path M36 added: the server-side
`player_logged_in` hook now binds `player: u128` alongside `name`, and
`colibri/join.rs` sends `GameCommand::ColibriPlayerJoined` with the canonical
hyphenated UUID and the joining name. The presence fallback is unchanged.

While the bridge is active, the worker reads `server_api_url` and resolves
`<server_api_url>/v1/links/{uuid}` with an unauthenticated GET. A 404 means the
player has not linked an account and is quiet; a service error is also quiet,
matching atpcraft's unavailable-result behaviour. The worker skips the local
player's DID because M30 already checks it. For another DID it resolves the DID
document, takes the `#atproto_pds` endpoint, and reads
`social.colibri.actor.profile` at rkey `self` with an unauthenticated
`getRecord`. A 200 means the profile exists; a 404 or the PDS's
`RecordNotFound` 400 means it is missing; every other result is quiet.

A missing profile emits `Event::ColibriRemoteProfileMissing { player_uuid,
player_name, url }`. The runtime queues atpcraft's two-line setup prompt in a
targeted queue. The tick hook drains that queue and calls `native_tell(uuid,
Text::literal(line))`, so the prompt reaches the joining player rather than the
local HUD. Server ticks now attach the sidecar and drain events too; a client
window is no longer required to receive worker events. The sidecar and its data
folder still have to be installed on the machine running the server.

Verified: eight `identity` tests cover the exact claim body and public link
lookup, 404-as-unlinked, service errors and UUID validation; the worker test
links `did:plc:remote` through the stub service, reads its missing profile over
`getRecord`, asserts the targeted event, then checks that an unlinked UUID is
quiet. Runtime tests pin the join command, event decoder, HUD line, prompt
lines and UUID round-trip; the injected hook itself is not compiled by
`cargo test`. No live identity service, PDS or Colibri AppView was contacted.

### M38: linked-account join gate

M15 could decide list membership and M35/M36 could link a UUID to a DID, but
nothing refused a join. M38 uses Horizon's pinned `player_login` hook, which
runs after the server accepts the connection but before the player is placed.
The hook binds `player: u128`, `cancel: &mut bool` and
`reason: &mut Option<Text>`; `gate/login.rs` asks the runtime for a refusal
and, when it gets one, cancels with that text on the disconnect screen.

`Worker::emit_join_gate_state` publishes `{enabled, list}` at startup and
after a settings save. The runtime caches it; an unknown state waits up to
3 seconds. With the gate off, login proceeds without a request. With it on,
the runtime sends `JoinGateCheck { request_id, player_uuid, player_name }` and
waits up to 6 seconds for the matching `JoinGateDecision`. The worker bounds
the whole identity, list and optional link-creation read at 5 seconds.

The worker reads `server_api_url`, resolves the UUID through
`GET /v1/links/{uuid}`, and checks the returned DID with the existing
`lists::check_gate` read against `join_gate_list`. A linked member is
allowed. An unlinked UUID, a non-member, an empty list, a missing service
URL, any service/AppView error, and a timeout are refused; this is atpcraft's
fail-closed rule. The M40 unlinked branch also creates a browser link inside
the same 5-second budget and appends its URL to the refusal; if that request
fails, the generic refusal is returned. With no sidecar attached the runtime
cannot know the gate is armed and stays out of the way. The gate runs on the
server thread only; the worker keeps all network and disk I/O.

This does not implement atpcraft's IP-bound `/mc` gate sessions. M39 supplies
the browser OAuth flow and M40 runs it from the refusal path, so an unlinked
player can link before being placed. M42 adds the periodic sweep that
disconnects an online player removed from the list.

Verified: a worker test links one UUID to a member, one to a non-member and
one to nothing, then asserts allow, refusal, the browser URL in the refusal
and fail-closed empty-list behaviour. Runtime tests pin the gate-state/decision
wire shapes and the synchronous refusal and allow paths. The injected
`player_login` helper was compiled in the staged pinned Horizon tree, where
all 13 `game::atmospheric` tests pass; `cargo test` alone does not compile it.
The staged Horizon-wide run still reports its own
`hook_points_stay_hookable` source-scan test because other injected helpers
contain macros; the M38 hook has none. No live identity service or AppView was
contacted.

### M39: browser self-service linking

M36 could only claim a code the operator issued, and M38's gate runs before a
refused player can type a command. M39 gives an already-connected player a
one-command browser flow without weakening the UUID binding. The game server
calls `POST /v1/browser/pairings` with `{player_uuid, player_name}` and
`Authorization: Bearer $ATMOSPHERIC_SERVER_LINK_TOKEN`. The service creates a
random 24-byte link token, stores `{uuid, name, expires_at}` in memory using
the lifetime from the creation request (M55), and returns
`<public_url>/link/<token>`. `server_link_token` is a new game setting;
without it, or without `ATMOSPHERIC_SERVER_PUBLIC_URL`/
`ATMOSPHERIC_SERVER_LINK_TOKEN` on the service, the flow is disabled with a
clear error. The browser never sends a UUID.

The player's worker reports `Event::BrowserLink { player_uuid, url,
expires_at }`. The runtime queues it for targeted `native_tell`, so only the
requesting player sees the URL; the HUD also shows it as a status line. Opening
the URL shows a handle form. Submitting it resolves the handle and DID, fetches
the DID document and PDS, discovers the authorization server, generates state,
a PKCE S256 verifier and a DPoP key, and sends a PAR request whose `dpop_jkt`
is the key thumbprint. The returned authorize URL uses the service's published
client metadata (`client_id = <public_url>/oauth-client-metadata.json`) and
redirects to `<public_url>/link/callback`. The callback requires the state,
issuer, code, DPoP token type, matching `sub` and the `atproto` scope. It then
consumes the link token and writes the link through the same atomic store M35
uses. State, verifier and DPoP private key are in memory only, single-use and
ten minutes.

The service now makes outbound requests for linking: the handle resolver
(`bsky.social` by default), the PLC directory or `did:web` host, the account's
PDS, its authorization server, and the token endpoint. It does not call the
AppView for this flow and does not store access or refresh tokens. A restart
invalidates browser link sessions. The OAuth client metadata and callback are
public; link creation is authenticated to the game server, and possession of
the one-time URL is the player's credential. M39 serves connected players;
M40 reuses the same creation request from the M38 refusal path so an unlinked
player can link before being placed.

Verified: server tests cover link-token authorization, URL shape, the browser
form, client metadata, callback state rejection and expiry; OAuth unit tests
cover metadata, the DPoP proof and PKCE; `identity` tests cover the game-server
request body and missing-token failure; runtime tests pin the command and event
wire shapes. The Docker test creates a link, fetches its form and reads the
metadata without contacting an external provider. The full OAuth start/finish
is covered against a local stub; no live PDS or authorization server was
contacted.

### M40: browser link from the join-gate refusal

M39's browser flow was only reachable from `/atmospheric link`, but M38 runs
before a refused player can type a command. M40 closes that gap without a new
server endpoint or a second link store. `gate/login.rs` already has the
joining player's name; it now passes it through `join_gate_refusal` and
`Command::JoinGateCheck` as `player_name`. The runtime adds the field to the
JSON command, and the worker uses it only for the identity service's existing
`POST /v1/browser/pairings` request.

When `join_gate_verdict` finds no link and `server_link_token` is non-empty, it
calls the same M39 creation path inside the existing 5-second gate timeout and
returns a refusal with the one-time URL appended. A failed creation, a missing
service URL, or a malformed response falls back to the generic unlinked
message, and the join still fails closed. The URL expires after `browser_link_ttl_seconds` (ten minutes by default, at
most one hour), is single-use, and is held only in the game's disconnect
reason and the service's memory. No new wire field, endpoint, or stored data was added beyond
the `player_name` field already carried by other identity requests.

`ServerConfig` also gained a test-only `oauth_resolver` override (defaulting to
the public Bluesky services) so the router-level OAuth test can point handle
and DID resolution at a loopback stub. The test drives the entire flow through
the real router: link creation, the handle form, PAR, the callback, PKCE
verification, DPoP proofs, persisted link lookup and state replay rejection.

Verified: the worker gate test asserts the URL appears in the refusal and that
`/v1/browser/pairings` was requested; the runtime test pins the command and
player-name field; the new `oauth` integration test covers the complete
browser flow and persistence. No live identity service, PDS or authorization
server was contacted.

### M42: online whitelist sweep

M38 only checks the list when a player logs in, so a player admitted before a
list edit stays until they reconnect. M42 is atpcraft's `startWhitelistSweep` /
`computeWhitelistKicks`: a periodic cached check that disconnects an online
player whose linked DID has left the list. M52 supplies the member cache and
its stale-refresh rule.

The sweep period is the new `join_gate_sweep_seconds` config key (default 60,
`0` disables). `emit_join_gate_state` now sends
`JoinGateState { enabled, list, sweep_seconds }`; a positive value is floored
at 60 seconds to match atpcraft's list-cache floor. The runtime caches the
period and, in the once-a-tick hook, throttles the sweep locally — the worker
never schedules it, so a server tick is the only clock. `join_gate_sweep_due`
is the pure throttle (armed, period positive, elapsed since the last send);
`join_gate_sweep(players)` records the time even for an empty server and sends
`Command::JoinGateSweep { players: [{player_uuid, player_name}, …] }` from
Horizon's `native_server_players()` helper.

The worker's `join_gate_sweep` reads the list through the M52 cache (a stale
entry refreshes once), then resolves each online UUID through
`GET <server_api_url>/v1/links/{uuid}` and compares the DID to the members. It
emits `Event::JoinGateKicks { players }` only when the list is non-empty and
the whole read finished inside a 30-second budget. A failed list read (or a
timeout) propagates and kicks nobody; a per-player lookup error or an unlinked
player skips that player. The tick hook drains the kicks and runs vanilla
`kick <name> Removed from the server's Bluesky whitelist.` through Horizon's
`native_run_command`, matching atpcraft's disconnect reason. The sweep reuses
the M38 public reads — no new endpoint, and no new stored data.

Verified: a worker test against the stub identity service and AppView sweeps a
member, a linked non-member and an unlinked player, asserting only the
non-member is reported, that a clean sweep and an empty player list are
silent, that a failed list read kicks nobody and that a disabled gate is a
no-op; a worker test pins the 60-second floor on the emitted period; runtime
tests pin `sweep_due` (off switch, inside/at/past the period), the kick queue
and the new command/event wire shapes. The injected `tick` helper was compiled
in the staged pinned Horizon tree, where the `native_server_players` and
`native_run_command` helpers resolve as `pub(super)`. No live identity service
or AppView was contacted.

### M44: generic record browser

`/atmospheric records <did|handle> <collection>` sends
`Command::FetchRecords { subject, collection }`: an unauthenticated viewer for
any public collection. It reuses the M13/M16 read path — resolve the account
and its PDS with `presence::resolve_player`, then `presence::list_records`
pages `com.atproto.repo.listRecords` (limit 100, bounded pages) against that
PDS — so there is no new endpoint and no new stored data.

`fetch_records` reports the total record count and the newest
`RECORDS_HISTORY_LIMIT` (5) records, matching the M16 chat-history display
limit. `listRecords` serves a collection newest-first; the newest few are
reversed into display order (oldest first) and serialized as compact JSON
strings. The worker emits
`Event::RecordsHistory { subject, via, collection, total, items }`; the game
side mirrors it and the HUD shows "atmospheric: 3 app.bsky.feed.post
record(s) for did:plc:… — newest: {\"text\":\"…\"}", with the JSON collapsed
to one line and truncated. Nothing is parsed or verified: the records are
arbitrary lexicons, and the only network call is the subject's PDS; the query
carries only the subject's DID and the collection name.

Verified: a worker test against the stub PDS seeds an `app.bsky.feed.post`
record and reads it back logged out, asserting the response fields and that
listRecords was unauthenticated; runtime tests pin the command/event wire
shapes, the HUD summary and the empty-collection wording. Real-PDS wire
check: `pds_check` with `ATMOSPHERIC_RECORDS="<did|handle>|<collection>"`.

### M45: linked-player display names and pronouns

`player_logged_in` sends `Command::CheckPlayerProfile { player_uuid, player_name }` next to M37's Colibri check (new runtime helper `check_player_profile`). On the worker, `check_player_profile` resolves the UUID with the M35 identity-service read used by M37 (`GET <server_api_url>/v1/links/{uuid}`), then reads the profile with M24's `profiles::fetch_profile`: `app.bsky.actor.getProfile` against `appview_url` plus the direct `app.bsky.actor.profile` record at rkey `self`, with the direct record's pronouns preferred. Both reads are unauthenticated and carry only the DID, so the feature works logged out and needs no per-player write token.

`profiles::nickname_for` composes the name atpcraft shows for a linked player: the display name reduced by M47 to atpcraft's valid Minecraft name, or the handle's username part, or the Minecraft name; then ` (pronouns)` when the profile has usable pronouns. The complete nickname is capped at `NICKNAME_MAX_LEN` (32) characters; an empty result or one equal to the Minecraft name emits nothing. The worker then emits `Event::PlayerProfile { player_uuid, player_name, nickname }`. The runtime queues it as `RemoteNickname` (bounded, 32 entries); the tick hook applies each with Horizon's `native_set_nickname(uuid, Some(name))`, which updates chat and the player list for the session. That helper is plain text, so atpcraft's gray chat-only pronoun suffix is not reproduced — the same values appear in both places. Every failure (unlinked player, failed link or profile read, unusable profile) is quiet and leaves the vanilla name untouched.

Verified: profile-unit tests pin display-name+pronoun composition, the vanilla fallback for pronouns only, no event when nothing changes, control/whitespace cleanup and the 32-character cap; runtime tests pin the command and event wire shapes, the empty HUD line and the nickname queue; a worker test against the stub identity service and AppView asserts the composed nickname for a linked player and silence for an unlinked one, reusing the M37 link path and M24 profile path. No live identity service, AppView or PDS was contacted.

### M46: lexicon documents on the identity service

`atmospheric-server` serves the five owned lexicon documents at `GET /lexicons` (an HTML index) and `GET /lexicons/{nsid}.json` (the schema). The documents are `at.atpcraft.player.chat`, `at.atpcraft.player.defs`, `at.atpcraft.player.skin`, `at.atpcraft.player.snapshot` and `at.atpcraft.server.profile`; they are compiled into the binary with `include_str!`, so a request performs no filesystem read and no network call. Only an exact base NSID resolves; unknown files, a missing `.json` suffix and traversal-looking names are 404. Responses carry `Access-Control-Allow-Origin: *` because browser ATProto tools read public lexicons cross-origin.

The schemas are authored from this project's record types (`records::ChatRecord`, `SnapshotRecord`, `SkinRecord` and `ServerProfileRecord`), so they describe the fields the M5/M6/M9/M10 writers emit: chat `sentAt`/`message`/`server`, the shared summary/aspect/endorsement definitions, snapshot version/aspects/endorsements, skin blobs/model/timestamp, and the server descriptor plus signing key. They are wire-compatible with atpcraft's published documents; no atpcraft file is copied. atpcraft prefixes the served NSID with its per-server signing key id because its web server holds that key; this service has no game signing key, so it publishes the base NSIDs.

Verified: integration tests fetch the index and every document through the real router, assert the JSON `id` and `lexicon` version, the content types and CORS header, and 404 for unknown, suffix-less and traversal paths. No new outbound request, credential or stored data.

### M47: atpcraft-compatible linked-player name sanitization

M45 originally passed the raw Bluesky display name through (up to 32 characters) and fell back to the vanilla name. atpcraft instead reduces it before applying: keep `[A-Za-z0-9_]` and ASCII whitespace, drop everything else, collapse whitespace runs into `_`, and cap at 16 characters; when nothing valid remains, use the handle part before the first dot; if that also fails, leave the original name. `profiles::minecraft_name` and `handle_name` implement those two steps, and `nickname_for` appends the plain-text pronoun suffix and caps the complete nickname at 32.

The pinned helper set still exposes only `native_set_nickname`, which is plain text and updates chat and the player list together; there is no separate list-name, display-name or chat-decorate hook in `native-hooks.json`. atpcraft's Tab name is the 16-character sanitized name and its chat line adds a gray pronoun suffix. This mod applies the sanitized name plus the same suffix as plain text to both, so the chat text matches apart from styling, while the Tab entry carries the suffix.

Verified: profile tests pin unicode/punctuation stripping, whitespace-to-underscore conversion (including whitespace exposed by removed characters), the 16-character cap, the handle fallback, the both-invalid vanilla fallback and the 32-character total; the worker test expects `AppView_Name (she/her)`. No live service was contacted.

### M48: signing-key server id in chat records

atpcraft's chat writer receives `signingKey.keyId` and writes it as `PlayerChatRecord.server`. This mod previously wrote `config.chat_server_id`, default `horizon-client`. `chat_server_id` is now an optional override: when blank (the default), `Worker::chat_record_server_id` loads or creates the persisted Ed25519 endorsement key through the existing M6 path and uses its key id. An explicit value still wins, so existing operators can keep a custom identifier. The key id algorithm is the one M6/M13 already use (`signing::key_id_of`), so chats, snapshots and skins now name the same server key.

Verified: the existing worker test pins the explicit override; a new stub-PDS test logs in, posts chat with no override, and asserts `record.server` equals the persisted `signing-key.json` key id. No new endpoint, credential or record shape.

### M49: server profile name and capabilities

atpcraft builds `ServerProfileWriter.Identity` with `name = atproto-server-name.ifBlank { bluesky-client-name }`, and `ServerCapabilities.of` derives `join-gate`, `player-data-sync`, `skin-sync` and `labeler` from the live feature wiring. This mod previously fell back to the account handle and defaulted `server_capabilities` to `player-data-sync`/`skin-sync` even though server-side player-data capture is not implemented.

`server_profile_name` now uses `server_name` when non-empty and `client_name` otherwise. `server_profile_capabilities` uses an explicit non-empty `server_capabilities` as an override; otherwise it advertises `skin-sync` (M9/M14 are implemented) and `join-gate` when the gate is armed. M59 later adds `labeler` while its declaration flag is on; M63 adds `player-data-sync` when `server_api_url` is configured. No new endpoint, credential or record shape.

Verified: config tests pin the `client_name` default and the empty capability override; worker unit tests pin the name fallback and the join-gate/skin-sync derivation with an explicit override. No live PDS was contacted.

### M50: proactive server-account token refresh

atpcraft arms `ServerAccountTokenRefresher` while the server runs and calls its token store every `server-account-token-refresh-seconds` (default 60, floored at 1). The store's expiry check skips the network until the access token is actually near expiry, so the steady-state tick is a cheap in-memory check; the point is that an idle server descriptor, Colibri service-token mint or future server-side write always finds a live token. This mod only called `ensure_server_oauth_fresh` before an on-demand server action, so a quiet period could make the next action pay the refresh round-trip.

`Worker` now carries `server_token_refresh_secs`, loaded from `server_account_token_refresh_seconds` on a resumed server session and after a server-account OAuth login. The worker loop owns a separate `tokio` interval whose first tick is one period away; each tick calls `refresh_server_token`, which is a no-op with no server account and otherwise delegates to the existing expiry-aware `ensure_server_oauth_fresh`. A completed server OAuth login recreates the interval so a changed setting takes effect without a restart. The period is floored at one second.

Verified: the config round-trip and older-settings tests pin the default and a non-default interval; a worker unit test pins the one-second floor. No live PDS was contacted. No new endpoint, credential or record shape; the token endpoint simply sees refreshes on a cadence instead of only on demand.

### M51: automatic server-profile publish

atpcraft re-publishes the descriptor inside the server-account login callback and calls `publishServerProfileOnStartup()` after the server starts. Both paths are best-effort: a missing account or invalid credentials skip the write, and a failed write does not fail the login or the server start. This mod required `/atmospheric server-account publish`, so a restart or a new server-account login could leave the public descriptor behind the configured name, capabilities, game version or signing key.

`Worker::auto_publish_server_profile` now calls the existing M25/M10 writer after a successful server-account OAuth login and after a persisted server session resumes at startup. It does not add a success event, matching atpcraft's silent best-effort publish; a failure is surfaced through the worker's normal `Failed` event without undoing the session. The manual command still works. The writer keeps its existing `getRecord`/`putRecord` pair, rkey `self`, `createdAt` preservation and M49 name/capability rules, so no record shape or endpoint changes.

Verified: the server-account OAuth worker test now expects the automatic upsert and the manual refresh, and a new startup-resume test creates a persisted server session, refreshes it through the stub authorization server, and asserts one descriptor `putRecord` with the refreshed access token and the default name/capabilities. No live PDS was contacted.

### M52: cached list members

atpcraft's `AtprotoListResolver` caches the member DIDs from `app.bsky.graph.getList` for `bluesky-list-cache-seconds` (default 300). `isMember` serves a fresh cache hit without a request, refreshes when the cache is stale, and refreshes once on a fresh miss before the final reject; `getMembers` reuses a fresh cache, and the whitelist sweep force-refreshes on its period. This mod fetched every page for every list read, gate decision and sweep.

The new `list_cache_seconds` setting (default 300; `0` always refreshes) controls a `ListCache` on the worker. It is keyed by the AppView and canonical list AT-URI, and its timestamp is taken after a successful read. `cached_list_members` returns the members plus whether they came from a fresh hit; `fetch_list` reuses that hit, `check_gate` refreshes once when a fresh cache misses the subject, and the sweep reuses the cache so the default 60-second sweep only performs a network read when the 300-second cache is stale. A failed read leaves the last successful cache intact and still fails closed at the gate. The sweep period remains the separate `join_gate_sweep_seconds`; the cache controls the list-read cadence.

Verified: config tests pin the default and a non-default TTL; a worker unit test covers the TTL boundary, `0` and key mismatch. The M11 test repeats a list read and asserts it still fetched only the original two pages; the M15 test asserts two pages for the first check, two more for a fresh cache miss, and two more for a different list. No live AppView was contacted. No endpoint, credential or record shape changed.

### M53: configurable OAuth scope

atpcraft exposes its requested scope as `bluesky-scope`: `effectiveScope` falls back to its granular default when the setting is blank, and the client metadata declares the same value. This mod always requested `atproto transition:generic` and only varied it by appending the Colibri grants, so an operator whose authorization server expects a narrower or granular scope had no setting for it.

`Config.oauth_scope` (default empty) is the new base. `oauth::scope_for(base, bridge_enabled, appview_did)` trims it, falls back to `DEFAULT_SCOPE` when blank, returns it unchanged when the bridge is off, and otherwise appends each Colibri repo/RPC grant only when the base does not already declare it, so a repeated build is idempotent. `scope_for_colibri` remains as the default-base wrapper. `start_oauth` now uses the configured base; `DEFAULT_SCOPE` itself is unchanged, preserving existing logins and the loopback client metadata shape.

A hosted `oauth_client_id` metadata document must declare the same custom scope, because the authorization server checks the client's declared scope against the pushed request. No endpoint, credential or stored record changes; a login made under a different scope may need to be repeated if the authorization server granted only the old one.

Verified: config tests pin the empty default and a non-default round-trip; OAuth unit tests cover the default fallback, trimming, a custom base without the bridge, and append-once behaviour. The existing stub flow tests still request `DEFAULT_SCOPE`. No live authorization server was contacted.

### M54: required OAuth grants for custom and server scopes

atpcraft's `effectiveScope()` appends the snapshot, skin and chat repo grants plus `blob:*/*` to any configured base, adds the RPG item grant while item discovery is on, and adds the Colibri message/membership grants while the bridge is on. Its server login uses `clientMetadataScope()`, which adds the server-profile repo grant, the RPG attestation grant, the Colibri stream grant and, when the labeler is on, the labeler-service and `identity:*` grants. M53 made the base configurable but appended only the Colibri message/membership grants, so a narrow custom scope could log in and then fail snapshot, skin, chat or descriptor writes; player logins also requested the stream grant they never use.

`scope_for` now appends `at.atpcraft.player.snapshot`, `at.atpcraft.player.skin` and `at.atpcraft.player.chat` (create/update/delete) and `blob:*/*` to a non-default base, once each, and keeps appending the Colibri message/membership grants when the bridge is on. `scope_for_server` adds `at.atpcraft.server.profile` (create/update/delete) and, when the bridge is on, `social.colibri.sync.subscribeEvents?aud=…`; it now also takes `labeler_enabled` and adds the M59 labeler-service declaration grant (M60 adds `identity:*` beside it). `start_oauth` picks it for `OAuthTarget::Server`. The mod's default `atproto transition:generic` already covers those repo and blob writes, so the grants are not appended to it and the default scope string is unchanged (atpcraft instead defaults to a granular scope). The RPG grant remains absent until #111.

A hosted `oauth_client_id` metadata document must declare the union used by its logins: the custom base plus these grants. No endpoint, credential or record shape changes; an existing session keeps its granted scope until it logs in again.

Verified: unit tests pin the custom-base grants, no duplication, the default's unchanged string, the player/server split and the server-only descriptor and stream grants. The config round-trip and older-settings tests still pass. No live authorization server was contacted.

### M55: configurable browser-link lifetime

atpcraft applies `bluesky-pairing-token-ttl-minutes` (default 10) to the in-game pairing token that opens its browser flow. This mod hard-coded the M39/M40 browser link to 600 seconds, and the authenticated create request had no lifetime field, so an operator could not shorten or lengthen the window.

`browser_link_ttl_seconds` (default 600) is now sent as the optional `ttl_seconds` field on `POST /v1/browser/pairings`. The server resolves it with the same `pairing_ttl` bound as operator codes (60–3600 seconds) and stores the result in the link's `expires_at`; an out-of-range value is a 400 rather than a silent clamp. The M40 refusal reports the configured window in whole minutes. The in-memory OAuth attempt still uses its own 600-second `BROWSER_TTL_SECS`, so changing the setting does not extend a sign-in that is already in flight.

The default is unchanged and an omitted field still uses the service default, so other clients of the endpoint keep working. No endpoint is added; one optional request field and one config key are. A hosted client is unaffected.

Verified: config tests pin the default and a non-default round-trip; the server unit test pins a 120-second stored lifetime, and the router test asserts a 120-second response plus a 400 below 60 and above 3600; the identity client test asserts the request field; the worker gate test asserts the default 600 is sent and the refusal still carries the URL. No live identity service was contacted.

### M56: label signing core (labeler part 1)

atpcraft signs every emitted label with a secp256k1 (K-256) key over the label's canonical DRISL/DAG-CBOR form, excluding `sig`, and publishes the compressed public key as a `did:key` `#atproto_label` verification method. This mod had only the M12 read side, so it had no encoder or signer to build the labeler service on.

`atmospheric-server`'s new `labeler` scope adds `Label` (`com.atproto.label.defs#label`) and `LabelSigner`. The signer loads a 32-byte secret or generates one, exposes the compressed public key and its multibase `did:key` (`0xe7 0x01` multicodec + base58btc), signs the SHA-256 digest of the unsigned canonical CBOR map and normalizes the 64-byte compact signature to low-S, and verifies it. The encoder writes only the subset a label uses (definite-length maps, text, unsigned integers, booleans) and sorts keys length-first then bytewise: `cts`, optional `cid`/`exp`, `neg` only when true, `src`, `uri`, `val`, `ver`. The `sig` field is deliberately not part of the signing input.

No endpoint, PLC operation or declaration record is added in M56; M57 adds the durable log, key persistence and the query/emit routes on top, M58 adds the live subscription stream, M59 adds the server-account declaration, M60 the PLC identity update and M61 the game triggers.

Verified: exact canonical byte vectors for a minimal label and one with all optionals (including key order and `neg`), the secp256k1 generator-point public key for secret 1, `did:key` prefix and multicodec round-trip, secret validation, deterministic 64-byte low-S signatures, tamper rejection and cross-key rejection. No network was used.

### M57: label log and queryLabels (labeler part 2)

M56 produced signatures but had nowhere to keep or serve them. M57 adds the server half needed before identity and game-trigger work: a durable append-only label log, an active-state index, an owner-only K-256 key file, an admin emission route, and the spec `queryLabels` read.

`Labeler` lives under `atmospheric-server`'s `labeler` scope. On open it validates the configured service DID (a `did:plc` or `did:web`), loads or creates `<data>/labeler/signing-key.json` in the atpcraft-compatible `privateKey`/`publicKey` base64 JSON shape with mode 0600, and replays `<data>/labeler/labels.jsonl`. Each line is `{seq, label}` where `label` is the spec shape and `sig` is DAG-JSON `{"$bytes": ...}`; sequence numbers start at 1 and survive restart. Malformed lines are skipped so a partially written last line does not prevent startup. The active index keeps the latest assertion per `(uri, val)`, removes it on a negation, and filters expired assertions. `cts` is stamped by the server, and a supplied `exp` must parse as RFC 3339.

`POST /v1/labeler/labels` is admin-token protected and accepts only `uri`, `val`, optional `neg`, `cid` and `exp`; `src` is the configured service DID. It signs the canonical CBOR and appends before serving. `GET /xrpc/com.atproto.label.queryLabels` is public; `uriPatterns` is required and accepts repeated or comma-separated values, `sources` filters by label `src`, `limit` defaults to 50 and clamps to 1–250, and `cursor` is the last sequence from the previous page. `*` and trailing-`*` patterns match atpcraft. Errors use the XRPC `{error, message}` envelope; a disabled labeler returns `NotEnabled` 503. The query response contains labels only (no sequence wrapper), with the cursor string when the page is full.

The service is enabled only when `ATMOSPHERIC_SERVER_LABELER_DID` is set; otherwise the routes return the disabled error and no key is generated. No declaration record, PLC operation or game trigger is added in M57; M58 adds the live stream, M59 the server-account declaration, M60 the PLC identity and M61 the game triggers. No outbound request is added — all reads and writes are local.

Verified: 32 server unit tests and 6 API tests, including key reuse and 0600 permissions, exact signed JSON shape and round-trip, log replay and sequence continuation, assertion/negation/expiry state, repeated/comma query parsing, admin auth, query filtering/pagination, persistence and input rejection. No live labeler or PDS was contacted.

### M58: subscribeLabels and public CORS (labeler part 3)

M57 could answer point-in-time queries but spec clients had no way to follow the log. M58 adds `GET /xrpc/com.atproto.label.subscribeLabels` as a public WebSocket over the same `Labeler` state, and closes the browser-consumer gap on the XRPC lane.

Each binary message is two canonical DAG-CBOR objects, matching the reference XRPC framing: a header `{t:"#labels", op:1}` followed by a body `{seq, labels:[…]}`. This service emits one label per frame. Inside the frame the signature is a CBOR byte string (`sig`), not the DAG-JSON `{"$bytes": …}` used by `queryLabels` and the on-disk log. Error frames use the header `{op:-1}` and a body `{error, message?}`.

A `cursor` query parameter selects the first sequence to replay; absent, invalid or 0 means the full history. The handler subscribes to the live broadcast before snapshotting the log, sends the backfill in sequence order, then forwards live labels while skipping any sequence already sent in the race window. A cursor beyond the durable tail gets a `FutureCursor` error frame and a normal (1000) `FutureCursor` close. Backfill replays the log, including expired entries; live fan-out skips expired labels, matching atpcraft. A subscriber that falls more than 1,024 labels behind is closed rather than blocking emission.

The public XRPC routes now answer permissive CORS: `Access-Control-Allow-Origin: *`, GET/OPTIONS, and `content-type`, `authorization`, `atproto-accept-labelers` and `atproto-proxy` request headers. No credentials are allowed or reflected.

No environment variable, record shape or outbound request is added. The endpoint is disabled with the rest of the labeler when `ATMOSPHERIC_SERVER_LABELER_DID` is unset.

Verified: exact canonical header and error-frame bytes, byte-string signature encoding, cursor parsing, live broadcast and expiry filtering, cursor-filtered backfill, and one loopback WebSocket test covering full backfill, `cursor=1` tail replay, live delivery, `FutureCursor` plus close, and the CORS preflight. No live labeler or PDS was contacted.

### M59: labeler service declaration (labeler part 4)

atpcraft publishes `app.bsky.labeler.service` at rkey `self` from its server
account after login, declaring the label values its labeler emits. M58 had the
serving path but no declaration, so clients had no metadata for the vocabulary.

`Config.labeler_enabled` (default `false`, matching atpcraft's
`bluesky-labeler-enabled`) gates the write. `social/labeler.rs` builds the record:
`policies.labelValues` lists the 11 curated IDs (`player`, `online`, `admin`,
`vip`, `donor`, `playtime-bronze`, `playtime-silver`, `playtime-gold`,
`dragon-slayer`, `monster-hunter`, `death-defier`), and
`labelValueDefinitions` gives each one `severity: inform`, `blurs: none`,
`defaultSetting: warn`, `adultOnly: false` and an English name/description. The
write path reads the existing record first to preserve `createdAt`, then puts
the declaration at `self`; a new record gets the current time, and the shared
record helper adds `$type`.

`Worker::auto_publish_labeler_declaration` runs after the M51 descriptor write on
a successful server-account OAuth login and when a persisted server session
resumes at startup. It is best-effort like the descriptor: a failure is reported
through the worker's normal `Failed` event and never undoes the session.
`server_profile_capabilities` advertises `labeler` while the flag is on.

`scope_for_server` takes the flag and appends
`repo:app.bsky.labeler.service?action=create&action=update&action=delete` only to
a narrow base. The default `atproto transition:generic` is unchanged, and no
`identity:*` grant is added.

No PLC operation updates the DID document's `#atproto_labeler` service or label
verification method, and no game trigger emits labels in M59, so the declaration
exists but clients cannot yet discover the labeler through the account identity;
M60 and M61 close both gaps. The
record lives on the PDS; nothing new is stored locally. Wire shapes come from
the public lexicon and the vocabulary IDs from atpcraft's configuration; no
atpcraft source was copied (its repository has no licence).Verified: unit tests pin the 11 IDs, every definition shape, `$type` and an existing `createdAt`; the worker's stub-OAuth login test asserts the declaration `putRecord` alongside the descriptor; OAuth tests pin the grant, its absence on the default broad scope and idempotence; config and capability tests pin the default-off flag and the conditional capability. No live PDS was contacted.

### M60: labeler PLC identity publication (labeler part 5)

The M59 declaration gives clients the vocabulary, but a labeler is discovered
through its account's DID document: `#atproto_label` publishes the K-256 key
that verifies each label and `#atproto_labeler` publishes the HTTP endpoint that
serves `com.atproto.label.*`. atpcraft merges both entries into its server
account's PLC operation. M59 deliberately stopped at the record, so this was the
last missing half of labeler discovery.

`atmospheric-server` now serves the labeler's public half at
`GET /v1/labeler/identity`: the service DID, the signing key's multibase
`did:key` and the public endpoint (null when `ATMOSPHERIC_SERVER_PUBLIC_URL` is
unset). All three are already public — the key verifies published labels and the
endpoint is what the DID document names — so the route carries no player data and
needs no admin token. It is disabled with the rest of the labeler when
`ATMOSPHERIC_SERVER_LABELER_DID` is unset.

`account/plc.rs` implements the update. The worker reads that identity, checks
the service DID matches the logged-in server account (a mismatch would rewrite
the wrong document), then reads the account's current operation from
`https://plc.directory/{did}/data`. It keeps every rotation key, alias,
verification method and service, adds or overwrites `atproto_label` and
`atproto_labeler`, and sends the operation to
`com.atproto.identity.signPlcOperation` on the account's PDS. The PDS signs it
with the account's identity key; the signed operation then goes to
`submitPlcOperation`. Empty arrays are omitted from the signing request so an
operation never clears an entry it did not set. A `did:web` account is rejected
with a clear error because its document is not updated through PLC.

Three commands expose the flow: `/atmospheric labeler-identity status` (a public
read of the current document, reporting whether each entry exists and whether
the published values match the service), `request` (asks the PDS to email a
one-time PLC token) and `submit <token>` (the write; an empty token is omitted,
for PDSs that do not require one). A narrow server-account OAuth scope now also
requests `identity:*` while `labeler_enabled` is on, because the PDS's
`signPlcOperation` authorizer requires it; the broad `transition:generic`
default already covers it. M59's note that no `identity:*` grant was added is
superseded by this milestone.

No new local storage and no label emission in M60; M61 adds the game triggers.
The declaration stays on the PDS; the DID document change is public identity
data. Wire shapes come from the
public lexicons and the PLC specification; no atpcraft code is copied.

Verified: unit tests pin the merge (preservation of the PDS service, rotation
key and alias, overwrite of stale labeler entries, omitted empty arrays,
`did:plc` enforcement, identity validation and the generated request types);
the router test pins the public identity route and its null endpoint; the worker
test drives a login against the stub PDS and asserts the sign request carries
the token plus both labeler entries alongside the existing ones, the signed
operation is submitted, and the status event follows; another worker test pins
the missing-server-account failure. No live PLC directory or PDS was
contacted.

### M61: labeler game triggers (labeler part 6)

M60 made the labeler discoverable, but nothing emitted labels when players
played. atpcraft asserts a rolling `player` badge and a dynamic `online` label
from its game events; this milestone ports those triggers onto the split
worker/service architecture.

The runtime adds two commands: `LabelerPlayerLeft { player_uuid }` from the
`player_logged_out` hook helper, and `LabelerSweep { players }` from the tick
hook. A join forces the next tick's sweep instead of sending a separate
command, so the newly placed player is captured immediately; otherwise the
sweep is throttled to once a minute. The worker resolves each UUID through
`GET <server_api_url>/v1/links/{uuid}`, reads the labeler's active labels with
`queryLabels`, and writes only the needed assertions and negations through the
admin `POST /v1/labeler/labels` route. A sweep asserts `player` (with a 30-day
`exp`), `online` and the labels the captured state justifies, and negates the
labels it no longer does; a disconnect negates `online`. Unlinked players and
failed lookups are skipped, and automatic failures stay silent rather than
interrupting play.

A new `Config.server_admin_token` supplies the admin bearer token; the triggers
are active only when `labeler_enabled` is on and both `server_api_url` and the
token are set. The startup path negates every active `online` label left by an
unclean game stop, through a wildcard `queryLabels` read.

`online` intentionally has no expiry here. atpcraft uses a 30-second expiry
because its labeler shares the game process; this labeler survives game
restarts, and the startup recovery clears stale presence, so an expiry would
only add churn.

No new endpoint or record shape. The only new outbound requests are the
identity link lookup, the active-label query and the admin emission, all against
the operator's configured identity service. No atpcraft code is copied.

Verified: `triggers` unit tests pin the RFC 3339 formatting, first-join actions,
the daily refresh threshold, logout negation and startup subject selection; two
worker tests against the stub identity service assert the admin token on the
emission, join presence, logout negation, stale-badge refresh, online
re-assertion and startup recovery; runtime tests pin the command wire shapes;
config tests pin the empty-token default. M62 extends these tests with the
status and milestone vocabulary. No live identity service or PDS was
contacted.

### M62: op-level and vanilla-stat label capture

M61 could assert only `player` and `online`, because the trigger had no op
level or statistics. Horizon's native server API exposes
`native_player_permission(uuid)` and `native_player_stat(uuid, type, key)`, so
the tick can capture them locally for every online player.

`LabelerPlayer` now carries `admin`, `play_time_ticks`, `mob_kills`, `deaths`
and `dragon_kills` alongside the UUID and name. `labeler_player_joined` sets a
request flag rather than sending a command, and `labeler_sweep_due` treats that
flag as due; the next tick builds a full `LabelerSweep` from
`native_server_players`, reading `minecraft:custom`/`minecraft:play_time`,
`minecraft:custom`/`minecraft:mob_kills`, `minecraft:custom`/`minecraft:deaths`
and `minecraft:killed`/`minecraft:ender_dragon`, plus the permission level. The
capture stays in the game and worker; only label values leave.

The evaluator adds `admin` when the op level is at least 1, the highest
playtime tier only (10, 100 and 1000 hours at 72,000 ticks each),
`dragon-slayer` at one dragon kill, `monster-hunter` at 1,000 mob kills and
`death-defier` at 100 deaths. It diffs against the active labels, so a lower
tier is negated when a higher one is asserted and a label whose threshold is no
longer met is negated. Only the nine owned labels are considered; manual `vip`
and `donor` grants are never touched. The `player` badge keeps its 30-day
expiry and daily refresh, `online` keeps its no-expiry presence semantics, and
logout still negates only `online`.

No new endpoint, credential or stored data. The captured fields travel only
between the game and its local worker; the labeler still receives just the
subject DID and label value. No atpcraft code is copied.

Verified: `triggers` unit tests pin the highest-tier rule, exact thresholds,
admin and milestone assertion/negation, and that `vip`/`donor` survive a sweep;
the worker test against the stub identity service asserts `admin`, gold,
`dragon-slayer`, `monster-hunter` and `death-defier` emissions for a fully
qualified player; runtime tests pin the new sweep payload. No live Minecraft
server, identity service or PDS was contacted.

### M63: RPG item discovery and player-data aspect capture/restore

This milestone follows atpcraft's **wire semantics only**; its source is
unlicensed, so no atpcraft file or code is copied. The supported aspects are
`inventory`, `ender_chest`, `equipment`, `advancements` and `stats`. The first
three use atpcraft's gzip-NBT wrappers: `{items: ListTag}` for the 36 main
inventory slots and the 27 ender-chest slots, and `{equipment: ListTag}` for the
five equipment slots encoded as `100`–`103` (helmet, chest, legs, boots) and
`-106` (offhand). Advancements and statistics are raw JSON. `core`, `ops`,
`scoreboard`, `team`, `effects`, `respawn`, `recipes` and `chat` are deliberately
not captured because Horizon has no complete round-trip setter for them; a
restore ignores them instead of pretending to be at full parity.

Capture and restore are manual and self-service. They run in the server tick,
not in a player turn, because Horizon rejects inventory access during a turn.
`/atmospheric player-data capture [player]` reads the target online player's
inventory, ender chest, equipment, statistics and advancements, encodes each
aspect and sends `PostPlayerData` to the worker. The worker calls the public
M35 link route and requires the player's UUID to map to **the worker's own
logged-in DID** before uploading each blob and publishing the existing signed
snapshot. `/atmospheric player-data restore [player]` sends `FetchPlayerData`;
the worker lists the account's snapshots, verifies the newest endorsement
against the local key or `trusted_server_keys`, downloads only the five
supported blobs (`com.atproto.sync.getBlob`, or inline bytes), caps each at
8 MiB and returns `PlayerDataRestore` for the next tick. The tick decodes the
aspects and applies them with `native_set_inventory_typed`,
`native_set_player_stats` and `native_set_player_advancements`; a malformed or
unverified aspect is never applied. This prevents an operator from writing a
guest's data into their own repo or restoring a stranger's snapshot onto a
linked player. M68 adds the automatic leave/join hooks and cross-server sync on
top of this manual path.

RPG discovery follows atpcraft's scan without its `customData` marker: it reads
the vanilla `minecraft:picked_up`, `minecraft:crafted`, `minecraft:used` and
`minecraft:broken` item statistics, plus renamed stacks in the inventory,
ender chest and equipment. Stacks with an `EQUIPPABLE` component are excluded
in both paths. A plain item key is the sanitized item id; a renamed stack adds
`~` and eight hex characters of SHA-256 over the **full** custom name, then
shortens long keys the same way atpcraft does. The game sends only
`{key, item_id, title, renamed}`. With `rpg_items_enabled`, the worker writes a
server-account `equipment.rpg.give` attestation at
`<recipient-did-without-did:>~<key>` and the player's `equipment.rpg.item` at
the same key, both with `kind: inventory`, the server-name context, atpcraft's
field caps and `putRecord` upserts. `rpg_items_max_per_scan` (default 10, at
least 1) caps one scan. The player link check is the same as capture: only the
logged-in account's own linked player can receive an item.

Protocol changes: `PlayerAspect`, `RpgDiscovery`, `PostPlayerData`,
`FetchPlayerData`, `PublishRpgItems` and the `PlayerDataRestore` event, plus the
RPG-aware OAuth scope helpers (`equipment.rpg.item` for the player and the
`equipment.rpg.give` write for the server account). No new endpoint or
credential. The worker continues to own disk and network I/O; the runtime stays
std-only.

Verified: RPG unit tests pin plain/renamed keys, long-key shortening,
deterministic give rkeys, key validation and record caps; OAuth tests pin the
RPG grants and their idempotence; a worker test pins `player-data-sync` in the
derived capabilities. `cargo check`, `clippy` and the workspace tests pass. The
game-side mixin bundle could not be staged in this environment because the
pinned `horizon-mixins` staging tool is not present, and no live Minecraft
server, identity service or PDS was contacted. atpcraft was used only as a
wire-behaviour reference.

### Achievement announcements (#181)

Advancement announcements are posted to Bluesky under a new default-off
`post_achievement_messages` setting, mirroring the existing death-message path.
The existing `system_message` injection in `mixins/atmospheric/death.rs`
recognizes `chat.type.advancement.*` translation keys in addition to `death.*`
and routes each to its own worker command. The game side sends
`GameCommand::PostAchievement { text }`, the protocol carries
`Command::PostAchievement { text }`, and the worker posts
`🏆 {text}` as an `app.bsky.feed.post` only while the setting is on. Both
settings are refreshed at login alongside the other cached flags, so an
operator can enable posts without a restart. The worker owns the gate and
drops the command before any network call while the flag is off, exactly as it
does for death messages. Both are broadcast lines, so every linked client that
opts in posts from its own account. Wire-shape pins cover the command on both
sides of the boundary; a worker PDS test checks the off/on behavior and the
exact post text. No new endpoint, record kind or credential.

### Automatic screenshot posting (#182)

Every successful F2 screenshot can now be posted as an image embed under a new
default-off `post_screenshots` setting. The existing `screenshot` HEAD injection
in `mixins/atmospheric/horizon-mixin.toml` (helper `shot.rs`) already observes the
finished save; it now also calls `post_screenshot_automatic(path, alt_text)` for
each F2 shot. It does not consult the in-memory "latest shot" slot, which belongs
to the manual `V` keybind, and it ignores world-icon saves (their path is not in
`ui.screenshot_messages`). The game-side command gains an `automatic` flag: the
manual keybind sends `false`, the F2 hook `true`. The worker drops automatic
`PostImage` commands before any file read or network call unless
`post_screenshots` is on, so calling the hook for every screenshot is cheap while
the setting is off. The manual `V` keybind is unaffected by the flag and always
posts. The flag is refreshed with the other cached settings at login. Wire-shape
pins cover `PostImage` on both sides of the boundary and the automatic command on
the channel; a worker PDS test checks the off/on behavior and the rendered embed.
No new endpoint, record kind, credential or stored data — the image file is still
read only at post time from the path the game saved.

### Image post targets (#183)

Image posts now use a configurable `image_post_target` (`bluesky`, `spark` or
`grain`, default `bluesky`) instead of always writing an
`app.bsky.feed.post`. The enum lives in `config`, is re-exported from the
protocol crate, and is sent as a string in `SaveConfig` so the std-only game
runtime validates it through the worker's `Config` serde. The worker caches the
target and refreshes it on every path that refreshes the other settings (login,
startup resume and config saves). Only the worker reads the screenshot and
performs the upload/record writes.

`actions::post_image` uploads the bytes once with
`com.atproto.repo.uploadBlob` and dispatches on the target. Bluesky keeps its
typed `app.bsky.feed.post` with an `app.bsky.embed.images` embed. Spark writes
`so.sprk.feed.post` with a `so.sprk.media.images` union, a
`so.sprk.media.image`, an optional `so.sprk.feed.post#captionRef` and the typed
`so.sprk.media.defs#aspectRatio`; captions are capped at 3000 bytes and 300
scalars, a conservative bound on the lexicon's 300-grapheme limit without a new
dependency. Grain writes three TID-keyed records: `social.grain.photo` with the
blob, alt text and required plain `{width, height}` `social.grain.defs`
aspect ratio; `social.grain.gallery` with a title taken from the caption, then
alt text, then `"Screenshot from Horizon"`; and `social.grain.gallery.item`
linking the photo into the gallery at position 0. The worker reports the
gallery URI for Grain. The gallery title and the Spark caption are truncated on
UTF-8 boundaries; Grain only accepts PNG screenshots with readable dimensions,
because its photo lexicon requires the aspect ratio before upload.

The worker enforces the per-target blob caps before reading/uploading the
bytes: 2,000,000 bytes for Bluesky and Spark (below Spark's 5 MB lexicon cap)
and Grain's exact 1,000,000-byte lexicon cap. A non-PNG for Grain fails before
any network call. The player OAuth scope helper appends only the selected
target's narrow repo grants when a custom base lacks `transition:generic`; the
default broad scope is unchanged, and recomputation at login is idempotent.

No endpoint, AppView call, credential or new persisted data is added: Spark and
Grain records go to the user's already configured PDS through the existing
`uploadBlob`/`createRecord` calls. The only new setting is `image_post_target`;
the image is still read at post time from the game's screenshot path. Unit
tests pin both upstream record shapes, caption/title truncation, the target
caps and the target-specific grants; sidecar PDS tests pin the Spark and Grain
writes and Grain's pre-upload rejection. Wire-shape tests cover `SaveConfig` on
both sides of the boundary. Upstream lexicons were read from the public Spark
and Grain repositories; no live service or identity was contacted.

### M69: server card (#191)

The server card is a liveness-checked server manifest the **server
account** (M25) republishes under one fixed record key
(`click.croft.atmospheric.server.card` at rkey `self`), so a freshly stamped record
is evidence the server is still publishing and every field stays plainly
attributable to its author: the card carries no signature block (repo
authentication only), and the liveness verdict says when the author last
republished, never that anyone vouched for the fields.

The milestone has two halves, both in the `card` scope at
`crates/atmospheric/src/card/`:

- **The typed record (`card::record`).** `CardRecord`/`CardPack` with caps:
  64-byte names, a 128-byte plain `host[:port]` address, population counts
  capped at 100 000, at most 32 packs each with an https `source` and a
  lower-hex sha-256, and `conduct` restricted to https or at://. `validate()`
  runs before any publish and on every read, and its failure message is
  reported as-is. The collection uses the owner's existing `click.croft`
  authority namespace; changing it changes the published collection.
- **The read side (`card::digest`).** `latest()` picks the structurally
  valid record (rkey `self` preferred, otherwise the newest `createdAt`),
  mirroring the M19 pick; `epoch_seconds()` is a dependency-free RFC 3339
  UTC parser (whole seconds, non-UTC offsets refused rather than
  shifted); and `liveness()` grades recency against the worker's clock —
  `Fresh` within an hour (`CARD_FRESH_SECONDS`), `Stale` after,
  `Unknown` when the timestamp does not parse. `fetch_card()` resolves the
  account and pages the collection unauthenticated.

Protocol changes: `PublishServerCard { players_online, players_max }`,
`FetchServerCard { subject }`, and the `ServerCard { subject, via, card }`
event; wire-shape pins and round-trip entries cover all three on both sides
of the boundary. No new endpoint or credential: the publish rides the server
account's PDS session and the read is unauthenticated.

The game side (the std-only mirror in `mixins/atmospheric/runtime.rs`) carries
a plain-data `CardRecord`/`CardPack`/`CardDigest` and `CardLiveness` verdict.
`/atmospheric server-card publish` publishes once on demand from the live
server's player count and slot ceiling; a server tick republishes every five
minutes (`CARD_PUBLISH_SECONDS`), gated on a player-slot ceiling so clients
stay silent. `/atmospheric card-of <did|handle>` reads another server's card
and shows its name, version, population, pack count and liveness on the HUD.
The worker fills identity fields from the settings file, stamps a fresh
`createdAt` per publish at the fixed rkey, and advertises the pinned Horizon
revision from `compatibility.json` as `engineRevision`. Packs declare none
yet: Horizon's signed-registry lookup for `.hpack` checksums is a later slice,
so the card honestly publishes an empty list rather than unverifiable claims.
The #196 pre-join display remains a separate slice.

### Counting Stones: verifiable in-game polls (#193)

Counting Stones is adjudication infrastructure, not social posting: one
server-signed tally record is the only public artefact. The protocol lives in
`crates/atmospheric/src/poll/` in four atoms:

- **`record`** defines `PollRecord` in
  `click.croft.atmospheric.poll.result`, with strict byte/count caps on the
  question and choices, an RFC 3339 UTC close and publish time, the lower-hex
  Merkle root, the participant DID list and the aggregate tally. Its signature
  covers a canonical JSON payload of every field except `signature` itself,
  using the same Ed25519 helpers as the atpcraft endorsements. The record
  carries its own public key, so a reader verifies it without trusting the
  publisher. The same `validate_poll_spec` runs when a box opens and whenever
  a record is read or published.
- **`tree`** builds a binary Merkle tree over `(DID, salted choice commitment,
  deterministic nullifier)` leaves, duplicating the last leaf at odd levels,
  and issues inclusion proofs. The nullifier binds the DID to the poll's
  derived id, so one DID cannot vote twice and a receipt cannot be replayed in
  another poll.
- **`ballot`** keeps `choice` and `salt` private in the worker, derives the
  public commitment/nullifier, and publishes the signed record plus one
  private `Receipt` per ballot. A receipt carries the salt, nullifier, leaf
  index and sibling hashes; `Receipt::verify` rebuilds the leaf from the
  published fields and walks the proof to the signed root. No receipt, salt,
  choice or nullifier is ever put in the tally record.
- **`digest`** resolves a subject, pages `listRecords` on their own PDS
  unauthenticated, picks the newest structurally valid record and reports the
  signature verdict. `FetchPoll` and `PollResult` carry that verdict to the
  HUD through the existing worker boundary.

The worker owns a `polls: HashMap` of in-memory ballot boxes. `OpenPoll`
validates the caller's spec and future close time; `VotePoll` resolves the
player's linked DID through the M35 public link map, enforces a single DID per
poll and an open box, and emits a `PollVote` event without the choice;
`ClosePoll` signs with the persisted worker endorsement key, writes one
`createRecord`, and keeps the receipts in memory; `PollReceipt` resolves the
caller's DID and returns only their own receipt. The signing key never leaves
the worker. The server can still omit a vote, but the participant list is its
claim of completeness and each included voter can check their receipt against
the signed root.

Protocol additions: `Command::{FetchPoll, OpenPoll, VotePoll, ClosePoll,
PollReceipt}` and `Event::{PollResult, PollOpened, PollVote, PollPublished,
PollReceipt}`. The std-only game mirror adds the matching plain-data
`PollDigest`, `PollRecord` and `PollReceiptData`, wire-shape tests and chat
commands `/atmospheric poll open|vote|close|receipt|result`. No new endpoint or
credential: reads use the subject's PDS and the existing public identity link,
and the write rides the logged-in account's PDS session.

### Warrants: cosmetic ranks from signed issuer records (#192)

Warrants are cosmetic credentials a player keeps in their own repo and an
operator chooses to recognise. They grant a name-plate line only — never a
join gate or gameplay capability — and the lexicon is documented with that
intent loudly. The protocol lives in `crates/atmospheric/src/warrant/` in two
atoms:

- **`record`** defines `WarrantRecord` (default collection
  `club.atmospheric.warrant`): issuer DID, subject DID, cosmetic `title`,
  RFC 3339 UTC `issuedAt` and optional `expiresAt`, the issuer's Ed25519 SPKI
  key and a signature. `validate_warrant_spec` applies the same byte/shape
  checks before signing and on every read. The signature covers a canonical
  JSON payload of every field except `signature` itself, using the same
  helpers as the poll tallies and atpcraft endorsements, so changing the
  title, subject, collection or expiry invalidates it. `is_active_at` refuses
  a future `issuedAt` and a past `expiresAt`, so a backdated grant cannot flash
  early and a time-boxed one stops on its own.
- **`digest`** resolves the subject and pages `listRecords` for each pinned
  collection on their own PDS unauthenticated. `verify_against` is the trust
  root: it accepts a record only when the issuer DID **and** collection match
  an allowlist entry, the signature verifies with the key the record carries,
  and that key equals the entry's pinned key byte-for-byte. A self-signed or
  unpinned record is worthless no matter how valid its signature. The subject
  must match the repo being read, and lines are sorted and de-duplicated for a
  stable HUD. An empty allowlist returns an empty digest without any
  `listRecords` call. `FetchWarrants` and `WarrantResult` carry the lines to
  the HUD through the existing worker boundary.

The allowlist is a worker-side config field, `warrant_issuers` — a list of
`{did, collection, public_key}` with `public_key` the base64 SPKI Ed25519 key.
It is `#[serde(default)]` and empty by default, so old settings files keep
loading and nothing is trusted until an operator pins it; the game process
never supplies an issuer or key, which is what stops self-attestation.
Revocation is absence from the repo: a deleted record simply disappears on
the next fetch, and `expiresAt` gives an issuer a time-boxed grant.

Protocol additions: `Command::FetchWarrants { subject }` and
`Event::WarrantResult { subject, via, warrants: Option<Vec<WarrantLine>> }`.
The std-only game mirror in `mixins/atmospheric/runtime.rs` adds a plain-data
`WarrantLine`, wire-shape tests and `/atmospheric warrants <did|handle>`.
Status text marks the lines cosmetic. No new endpoint or credential: the read
is unauthenticated against the subject's PDS and nothing is written.

### Verified handle plaques (#185)

Unlike a linked badge, a verified plaque proves the handle cryptographically.
The protocol lives in `crates/atmospheric/src/account/proof.rs`. A
`Challenge` is a random 128-bit nonce plus the Unix time it was issued. The
linked session writes one `at.atpcraft.verify.nonce` record at the nonce rkey
to force a fresh repository commit; its only fields are `nonce` and
`createdAt`, never profile or gameplay data. Verification then:

- resolves the subject to a DID and `#atproto_pds` endpoint and requires it to
  be the logged-in account;
- reads `com.atproto.sync.getLatestCommit` for the CID and revision, then
  `com.atproto.sync.getBlocks` for that CID, and walks the CAR v1 frame to the
  commit block;
- re-encodes the unsigned commit as canonical DRISL/DAG-CBOR (map keys sorted
  by UTF-8 length then bytes, CID links as tag 42, `prev` present even when
  null) and verifies its ECDSA signature with the `#atproto` multikey from the
  PLC document, rejecting high-S signatures;
- checks the commit revision (a TID) is newer than the challenge and within
  `MAX_COMMIT_AGE_SECS` (300), bounding the replay window; and
- confirms the DID document's `alsoKnownAs` handle resolves back to the same
  DID.

This proves **PDS repository custody, not human custody** — the HUD text says
so, and the branch never writes profile data or gates gameplay. Protocol
additions: `Command::VerifyHandle { subject }` and
`Event::HandleProofResult { subject, proof: Option<HandleProof> }`. The
std-only mirror adds a plain-data `HandleProof`, wire-shape tests and
`/atmospheric verify <did|handle>`. A key-type variation (only K-256 multipkeys
are accepted) or a handle change mid-session fails closed.

### M68: automatic player-data sync

M68 makes the M63 capture/restore hooks automatic and cross-server. Two new
game-side injections drive the same worker commands as the manual commands:

- **Leave.** A raw `HEAD` injection at
  `horizon_client::game::native_hooks::Game<'_>::native_player_logging_out`
  captures the five M63 aspects while the player is still in their level and
  still listed in `native_server_players` — the only logout seam with live
  inventory, statistics and advancement state. `player_logged_out` is too late:
  the player has already been saved and removed. The aspect encoder moved into
  a `[[module]]` (`crate::game::atmospheric_playerdata`) so the logout helper and
  the tick's manual-capture path share exactly one implementation; a helper
  injected at one hook cannot call a helper injected at another by name.
- **Join.** A `HEAD` injection at `player_logged_in` sends `FetchPlayerData`;
  the existing `PlayerDataRestore` tick handler applies the returned aspects
  after placement, so no new apply path is needed.

Both commands carry `automatic: true` (the manual commands pass `false`),
mirroring the `PostChat { automatic }` precedent. A new default-off config key,
`player_data_sync_enabled`, gates only the automatic commands in the worker,
*before* any OAuth refresh or network call: when it is false, hook-originated
commands are silently dropped with no event and no PDS request; explicit
`/atmospheric player-data` commands always run. When the gate is open, automatic
commands use the same status/failure events as explicit ones. No new event,
endpoint or credential is added.

Cross-server restore reuses the M63 verification path unchanged. The worker
lists the logged-in account's snapshot collection, picks the highest version
whose endorsement verifies against the local signing key **or a
`trusted_server_keys` peer**, and downloads only the five supported blobs. A
peer server's signed snapshot is therefore portable between servers that trust
each other's keys, without a new route or shared credential. Full `core` NBT
restore is still out of scope — Horizon exposes `native_player_compound` but no
complete round-trip setter for the whole player compound.

One wire mismatch surfaced while pinning this slice: the std-only runtime
hand-rolls `SnapshotSummary` with snake-case struct fields, but the worker's
serde struct is `#[serde(rename_all = "camelCase")]`. `summary_json` now emits
`xpLevel`/`gameMode`, and both the runtime encoder test and a new worker
deserialization test pin the exact bytes. `PlayerDigest` event decoding remains
snake-case and is unchanged.

Verified with the layout scripts, `cargo fmt --check`, `cargo clippy
--workspace --all-targets -- -D warnings` and the full workspace test suite,
including worker tests that prove automatic commands are silent with the gate
off and reach the normal link check with it on. The mixin helpers pass
`rustfmt --check`. The bundle could not be staged in this environment (no
`horizon-mixins` CLI in `PATH`), and no live Minecraft server, identity service
or PDS was contacted; atpcraft served only as a wire-behaviour reference.

### M66: profile avatar and banner uploads

`/atmospheric profile-set avatar|banner <path>` reads a local image from the
machine running the game and writes it to the logged-in account's
`app.bsky.actor.profile` record. The worker validates the file's magic bytes,
accepts only `image/png` and `image/jpeg`, and enforces the profile lexicon's
1,000,000-byte cap before any network call. It fetches the current record,
uploads the bytes with `com.atproto.repo.uploadBlob`, converts the returned
blob reference to JSON and merges it into the existing record. Every other
field survives: the other image, `displayName`, `description`, `pronouns`,
links, pinned post and `createdAt`. The merged record is written with
`com.atproto.repo.putRecord` and PDS validation on, and the result is reported
as `Event::ProfileUpdated` with the field and record URI.

No new endpoint or config key is added: the existing `blob:*/*` player grant
covers the upload, and the runtime still carries only strings. The image path
is read by the worker, the only layer allowed to touch disk.

Verified with merge tests (one blob replaced, the other and every text field
preserved, text fields rejected), runtime wire-shape tests, and a stub-PDS
worker test that checks both uploads, both validated `putRecord` bodies, the
returned blob sizes and the preserved fields. The real-PDS check uploaded an
avatar and a banner, confirmed each write kept the other image and every
non-image field, then restored the original images from the Bluesky CDN after
the PDS garbage-collected the superseded blobs; the restored image CIDs
differ, while all non-image fields matched the pre-check record exactly.

### M65: profile-field editing

`/atmospheric profile-set <name|description|pronouns> [text]` writes one text
field of the logged-in account's `app.bsky.actor.profile` record at rkey
`self`. The worker reads the current record with an authenticated
`com.atproto.repo.getRecord`, merges the field into the existing JSON object,
and writes it back with `com.atproto.repo.putRecord` and PDS validation on.
Every other field is preserved: the avatar and banner blobs, links, pinned
post and `createdAt`. A missing record starts with the profile `$type` and a
fresh `createdAt`; an empty value removes the field. `name` maps to the
record's `displayName`.

The write uses the player's existing agent, so it works with an app-password
session and with OAuth. A narrow custom OAuth scope gains
`repo:app.bsky.actor.profile?action=create&action=update`; the broad default
`transition:generic` already covers it. No new endpoint, config key or stored
data is added, and the runtime stays std-only. The result is reported as
`Event::ProfileUpdated` with the field and record URI.

Verified: profile merge tests cover preservation, clearing, a new record and
an unknown field; a stub-PDS worker test checks the authenticated get, the two
putRecord bodies (update and clear), `validate: true`, and that the avatar and
`createdAt` survive; runtime tests pin the new command and event wire shapes.
The real-PDS check updated `pronouns` on the test account, verified the stored
record, and restored the original value.

### M64: browser skin upload and signed publishing

Issue #113 has two halves. The in-game half is blocked: the pinned Horizon
native API exposes no skin or texture setter, and the project rule forbids
hand-editing generated hooks or reaching into unexported game internals. M64
therefore implements the browser half only.

`/atmospheric skin upload` sends `Command::StartSkinUpload` for the acting
player. The worker requires `server_api_url` and `server_link_token`, then calls
`POST /v1/browser/skins` with the player UUID, name and configured link
lifetime. The service refuses the request unless the UUID already has a link;
it returns a one-time `/skin/{token}` URL that the game shows only to that
player. Unlike the pairing flow, the browser never chooses or supplies the
UUID.

The page accepts a PNG up to 1 MiB and checks the PNG signature; the model is
`default` or `slim`. It then starts a browser OAuth login with the same PAR +
PKCE + DPoP client, but a distinct `/skin/callback` redirect declared in the
client metadata. The resulting session is kept only for the publish: the
callback requires the signed-in DID to equal the linked DID, uploads the PNG
with `com.atproto.repo.uploadBlob`, reads the existing record with
`com.atproto.repo.getRecord`, builds the `at.atpcraft.player.skin` record with
the game worker's Ed25519 key (`ATMOSPHERIC_SERVER_SKIN_KEY`), preserves the
existing cape and every other server's endorsement, and upserts at rkey = key
id with `com.atproto.repo.putRecord`. The upload link is consumed only after a
successful write; tokens, verifiers, DPoP keys and uploaded bytes are in-memory
and dropped after the callback.

The server signs with the `atmospheric` crate's `SkinRecord` and
`signable_payload`, so a browser-written record verifies exactly like one from
`/atmospheric skin`; no atpcraft code is copied. The record builder treats a
malformed existing record as absent and replaces only this server's
endorsement. In-game application of the published skin is still not possible;
issue #113 tracks that upstream blocker.

Protocol and configuration: `Command::StartSkinUpload`,
`Event::SkinUploadLink`, the `/atmospheric skin upload` command and tick
delivery, and the optional `ATMOSPHERIC_SERVER_SKIN_KEY` environment variable.
No worker-side network path changed, and the runtime remains std-only.

Verified: router tests cover link-token auth, linked-player gating, disabled
signing, the upload page, invalid PNG rejection and the client-metadata
callback; an end-to-end test against a stub PDS resolves the handle, completes
PAR/PKCE/DPoP and verifies the uploaded blob, the rkey, the record shape and
the Ed25519 endorsement. Unit tests cover PNG validation, one-time upload state
and record construction. `cargo fmt`, `clippy -D warnings` and the workspace
tests pass. No live Minecraft server, PDS or OAuth provider was contacted.

### M14: player skin reads (`at.atpcraft.player.skin` + `getBlob`)

`/atmospheric skin-of <did|handle>` sends `Command::FetchPlayerSkin` — the
read side of atpcraft's skin sync, counterpart to the M9 write:

1. resolve the account and its PDS exactly as the M13 presence read
   (`presence::resolve_player`), then page `listRecords` for
   `at.atpcraft.player.skin` unauthenticated against that PDS;
2. `skins::latest_skin` takes the parseable record with the newest
   `timestamp` (skin records have no version counter), parsed as
   `records::SkinRecord` the way atpcraft's `fromJson` does (known model,
   valid timestamp);
3. `skins::verify_skin` verifies the endorsements with the shared
   `records::verify_endorsements` over `skins::skin_payload` — the canonical
   `{did, skinCid?, capeCid?, model, timestamp, server}` rebuilt from raw
   JSON, with a unit test asserting parity with the M9 writer's
   `SkinRecord::signable_payload` (asset keys omitted when absent);
4. only when the verdict is `Verified`, the worker downloads the skin/cape
   blobs via unauthenticated `com.atproto.sync.getBlob?did=&cid=` on the
   subject's PDS (`skins::get_blob`), rejects non-PNG content and anything
   over the lexicon's 1 MiB cap, and saves the bytes under
   `<storage>/skins/<cid>.png` (worker owns disk I/O). Unverified records
   are reported with their verdict but their blobs are never downloaded or
   persisted.

The worker emits `Event::PlayerSkin { subject, via, skin }` with a
`skins::SkinDigest` (model, timestamp, verdict, per-blob CID/mime/size and
the saved path); the game side mirrors it as plain data (boxed) and the HUD
summarizes it ("atmospheric: did:plc:… skin model slim — skin bafkrei…
saved to …, cape none — verified by keyid…"). Wire shapes come from the
lexicons only — no code is copied from atpcraft (its repo carries no
licence).

Verified: `skins` unit tests (payload parity with the M9 writer, asset-key
omission, all four verdicts, latest-by-timestamp, garbage rejection,
digest fields, `getBlob` against a stub returning PNG bytes) and worker
tests against the stub PDS (which now serves `getBlob` with a binary body):
a posted skin reads back verified by the persisted key with the blob saved
under `skins/` and byte-identical, a foreign key trusted via
`trusted_server_keys` verifies logged out, a tampered record reports
`BadSignature` and triggers no `getBlob`, a record-URI subject fails before
any HTTP call, an empty repo reports `None`, and both the `listRecords` and
`getBlob` requests are asserted unauthenticated. Real-PDS wire check:
`pds_check` with `ATMOSPHERIC_FETCH_SKIN`.

### M15: the join-gate decision (`gate`)

`/atmospheric gate <list> <did|handle>` sends `Command::CheckGate` —
atpcraft's `BlueskyJoinGate` membership decision as a client-side read;
M38 adds the server-side enforcement at `player_login`. `lists::check_gate`
resolves the
list reference (a bsky.app URL's handle owner via the directory service —
`resolve_list_reference`, which the M11 `fetch_list` now shares) and the
subject (the M12 subject parser/resolver; record URIs are rejected), pages
`app.bsky.graph.getList` unauthenticated on the configured AppView, and
compares DIDs into a `GateOutcome { list, subject, allowed }`. Fail-closed
like atpcraft: any resolution or list error propagates, and the worker emits
`Event::Failed` with the reason — the deny-with-reason, never a silent
allow. Works logged out.

The worker emits `Event::GateDecision { list, subject, allowed }`; the game
side mirrors it and the HUD shows "atmospheric: gate <list> — <did>
ALLOWED|DENIED". Wire semantics from the lexicons only — no code copied from
atpcraft (its repo carries no licence).

Verified: `lists` unit tests (both-sides resolution through a stub handle
service + AppView, member/non-member, record-URI and bad-reference errors)
and worker tests against the stub PDS (allowed via a DID member, denied via
a handle subject, the URL owner resolved, getList asserted unauthenticated,
both failure paths). Real-AppView wire check: `pds_check` with
`ATMOSPHERIC_GATE` / `ATMOSPHERIC_GATE_SUBJECT`.

### M16: chat-history reads (`at.atpcraft.player.chat`)

`/atmospheric chat-of <did|handle>` sends `Command::FetchChat` — the read
side of the M5 chat mirror: atpcraft mirrors each in-game chat line to the
player's own repo as a TID-keyed `at.atpcraft.player.chat` record, and this
command reads that history back.

1. resolve the account and its PDS exactly as the M13 presence read, then
   page `listRecords` for the chat collection unauthenticated against that
   PDS (`presence::list_records`, shared with M13/M14);
2. `chats::history` parses each record as `records::ChatRecord`
   (`sentAt`/`message`/`server`; `$type` ignored), skips malformed ones,
   orders by `sentAt` (the TID rkeys order the same way), and returns the
   total count plus the newest `HISTORY_LIMIT` (5) digests, each with the
   message truncated to 80 chars for the HUD;
3. chat records are **unsigned by design** — the lexicon has no endorsement
   fields — so there is no signature to verify; the event and HUD line say
   so explicitly.

The worker emits `Event::ChatHistory { subject, via, total, items }`; the
game side mirrors it as plain data and the HUD shows "atmospheric: 12 chat
record(s) for did:plc:… (unsigned) — latest from srv: \"hello\"". Wire
shapes come from the lexicons only — no code is copied from atpcraft (its
repo carries no licence).

Verified: `chats` unit tests (ordering, newest-N selection, garbage
skipping, message truncation) and worker tests against the stub PDS (a
seeded record reads back logged out with listRecords asserted
unauthenticated; two posted chats then appear ordered after it; an empty
repo reports zero; a record-URI subject fails before any HTTP call).
Real-PDS wire check: `pds_check` with `ATMOSPHERIC_FETCH_CHAT`.

### M19: server-profile reads (`at.atpcraft.server.profile`)

`/atmospheric server-of <did|handle>` sends `Command::FetchServerProfile` —
the read side of M10: an account advertises its server descriptor in an
`at.atpcraft.server.profile` record at rkey `self` (identity fields, versions,
capabilities, operator info, and a `signingKey` block), and this command reads
it back.

1. resolve the account and its PDS endpoint exactly as M13/M14/M16, then page
   `listRecords` unauthenticated for `at.atpcraft.server.profile` against that
   PDS (`servers::list_records`);
2. `servers::latest` selects the structurally valid record at rkey `self`
   (preferred), or falls back to the newest structurally valid record by
   `updatedAt`; malformed records are skipped rather than fatal;
3. `servers::verify` verifies the `signingKey` block: because the profile
   record carries no signature over itself (repo-authenticated), verification
   means checking whether the published `keyId` is one you trust — the mod's
   own Ed25519 key or an entry in `trusted_server_keys` — and that its
   published `publicKey` matches that key. Verdicts are `Trusted { key_id }`,
   `Unknown`, `KeyMismatch { key_id }`, or `Malformed`;
4. `servers::digest` builds the `ServerDigest` with identity fields, versions,
   capabilities, operator handle, and trust verdict.

The worker emits `Event::ServerProfile { subject, via, profile }`; the game
side mirrors it as plain data and the HUD shows "atmospheric: <name> — MC <version>,
N capabilit(y/ies), key <id> trusted/unknown/mismatched" (or "atmospheric: no server
profile for <subject>").

Verified: `servers` unit tests (structural validation, `self` rkey preference,
newest-record fallback, key trust verdicts, HUD digest) and worker tests against
the stub PDS (empty repo reports none, seeded record reads back logged out with
unauthenticated `listRecords`, trusted key verifies, unknown and mismatched
keys report proper verdicts, record-URI subject fails before HTTP).

### M20: social-graph reads (`app.bsky.graph.get*`)

`/atmospheric follows|followers|blocks|mutes <did|handle>` sends
`Command::FetchGraph { kind, subject }`. This is a pure public read, so it
works logged out:

1. `graph::GraphKind::from_key` rejects an unknown relation before any HTTP
   call (the worker reports a `Failed` event);
2. `graph::resolve_subject` accepts a DID or handle (not a record URI) and
   resolves handles through the same directory service as OAuth login;
3. `graph::fetch_graph` pages `app.bsky.graph.getFollows`, `getFollowers`,
   `getBlocks` or `getMutes` on the configured AppView (`appview_url`,
   default `https://public.api.bsky.app`) with `limit=100`, following the
   cursor up to a defensive 200-page cap. Each profile view contributes its
   DID, handle and display name; views without a DID are skipped.

The worker emits `Event::SocialGraph { kind, subject, via, members }`; the
game side mirrors it as plain data and the HUD shows "atmospheric: follows of
did:plc:… via <appview>: N account(s) — a.test, b.test +N more" (or
"atmospheric: no mutes for did:plc:…"). No credentials are sent and nothing
about the reader goes out beyond the subject DID being looked up.

Verified: `graph` unit tests (kind round-trip and endpoint routing, two-page
pagination with a DID-less view skipped, HTTP error reporting) and a worker
test against the stub AppView (all four relations, follows paged twice,
every read unauthenticated, an unknown kind fails before HTTP).
Real-AppView wire check: `pds_check` with `ATMOSPHERIC_GRAPH`.

### M21: interactions (`app.bsky.feed.like` / `repost` / a reply post)

`/atmospheric reply <at-uri> <text>`, `/atmospheric like <at-uri>` and
`/atmospheric repost <at-uri>` send `Command::Reply`, `Command::Like` and
`Command::Repost`; success reuses `Event::Posted { uri }`.

1. `interactions::resolve_post` accepts an `at://…/app.bsky.feed.post/<rkey>`
   URI or a bsky.app post URL, resolves a handle owner through the directory
   service, and rejects a non-post record before any PDS read.
2. `interactions::fetch_ref` fetches the target with an **unauthenticated**
   `com.atproto.repo.getRecord` from the author's PDS. The response's `cid`
   and the requested URI become the strong ref.
3. `interactions::reply_targets` uses the target's own `reply.root` when it
   is itself a reply, otherwise the target is its own root — so replies join
   the correct thread.
4. The worker writes to your repo with `com.atproto.repo.createRecord`:
   `app.bsky.feed.like` / `app.bsky.feed.repost` (`subject` strong ref), or an
   `app.bsky.feed.post` with `text`, `createdAt` and `reply: {root, parent}`.
   Keyboard-bound and command-originated writes both go through the same
   logged-in agent (OAuth tokens are refreshed first when needed).

README "Network use" documents the four endpoints and every field. No
atpcraft code is involved; the record shapes are the public Bluesky lexicons.

Verified: `interactions` unit tests (post-URI parsing, root inheritance,
record shapes, unauthenticated `getRecord`, non-post rejection before I/O) and
a worker test against the stub PDS (login, then like/repost/reply create the
right records with the right strong refs; target reads are unauthenticated; a
non-post target writes nothing). Manual check: `pds_check` with
`ATMOSPHERIC_LIKE`, `ATMOSPHERIC_REPOST` or `ATMOSPHERIC_REPLY`.

### M23: social-graph writes (`app.bsky.graph.*`)

`/atmospheric follow|unfollow|block|unblock|mute|unmute <did|handle>` sends
`Command::GraphWrite { action, subject }`. Every action needs a session and
resolves the subject to a DID before it writes:

1. `graph::GraphAction::from_key` rejects an unknown action, and the worker
   refuses a target equal to the logged-in DID before any HTTP call.
2. `follow` and `block` create a TID-keyed `app.bsky.graph.follow` / `block`
   record with `subject` and `createdAt` via `com.atproto.repo.createRecord`.
3. `unfollow` and `unblock` page `com.atproto.repo.listRecords` for the
   matching collection on your own PDS (public repos serve it without auth),
   select the records whose `subject` is the target DID, and delete each with
   `com.atproto.repo.deleteRecord`. No matching record is a `Failed` event
   ("you are not following/blocking …").
4. `mute` and `unmute` call the AppView's `app.bsky.graph.muteActor` /
   `unmuteActor`. Mutes are private state, not repo records.

The worker emits `Event::GraphChanged { action, subject }`; the game side
mirrors it as plain data and the HUD shows "atmospheric: followed
did:plc:…" (or the matching past tense). The sidecar wire shapes are pinned
in `lib.rs` and the runtime encode/decode tests.

README "Network use" documents the new endpoints and fields, including the
unauthenticated own-repo `listRecords` lookup. No new data is stored on disk.

Verified: `graph` unit tests (action round-trip, record shapes, subject
matching) and a worker test against the stub PDS (all six actions, follow and
block creation with the target DID, old and new records deleted, mute calls
authenticated, a missing relationship failing with its reason, and a
self-action failing before any HTTP request). Native staging compiles the
mixin command parser; the network calls themselves were not re-run against a
real PDS in this change.

### M17: the sidecar worker process (issue #2 option 3, #40)

The bundle cannot stage the worker crate into the game workspace (see
above), so the game attaches a **worker process** instead:

- `crates/atmospheric/src/bin/atmospheric-worker.rs` wraps the existing
  `worker::spawn` in a stdio protocol: one JSON `Command` per line on stdin,
  one JSON `Event` per line on stdout (serde's externally tagged default;
  the shapes are pinned by unit tests in `lib.rs`). `--data-dir` (or
  `ATMOSPHERIC_DATA_DIR`) selects the storage folder; `--no-browser` keeps
  tests headless. Stdin EOF stops the worker — the game exiting closes the
  pipe — and unparseable lines come back as `Event::Failed`.
- Game side, `runtime.rs` stays std-only: a hand-rolled minimal JSON
  parser/writer encodes `GameCommand` and decodes `ModEvent` in exactly that
  wire format (the `Settings` → `Config` mapping relies on the container-
  level `#[serde(default)]`; the flat snapshot `x/y/z` map onto the nested
  `position` object). `ensure_attached()`, called once per tick by the
  `tick` helper, finds the binary (`ATMOSPHERIC_WORKER_BIN`, else
  `atmospheric-worker[.exe]` next to the game executable; data dir
  `ATMOSPHERIC_DATA_DIR`, else `<exe dir>/atmospheric-data`), spawns it with
  piped stdio, and runs two bridge threads — commands to the child's stdin,
  the child's stdout lines to the event channel — plus a reaper thread.
  With no binary found, sends stay no-ops and the HUD stays hidden, exactly
  as before.

Verified: crate-side `tests/sidecar.rs` spawns the real bin
(`CARGO_BIN_EXE_…`) and checks the `{"Session":"LoggedOut"}` handshake, a
config-less `Login` failing without network, a garbage line, `SaveConfig`
writing `config.json`, and a clean `Shutdown` exit; runtime tests pin the
encode/decode shapes against the serde-pinned strings and run
`spawn_sidecar` against a fake worker script proving both directions.

### Manual check against a real PDS

`crates/atmospheric/examples/pds_check.rs` spawns the worker exactly as the
glue will, with a throwaway settings folder in the system temp dir, and
prints every event:

```sh
ATMOSPHERIC_PDS_URL="https://your-pds.example" \   # the account's own PDS, not always bsky.social
ATMOSPHERIC_IDENTIFIER="you.example" \
ATMOSPHERIC_APP_PASSWORD="xxxx-xxxx-xxxx-xxxx" \
ATMOSPHERIC_POST_TEXT="hello from atmospheric-horizon" \      # optional
ATMOSPHERIC_POST_IMAGE="/path/to/screenshot.png" \            # optional, M3
ATMOSPHERIC_POST_CHAT="hello from in-game chat" \              # optional, M5
ATMOSPHERIC_POST_SNAPSHOT=1 \                                  # optional, M6
ATMOSPHERIC_POST_SKIN="/path/to/skin.png" \                    # optional, M9
ATMOSPHERIC_POST_CAPE="/path/to/cape.png" \                    # optional, M9
ATMOSPHERIC_SKIN_MODEL="slim" \                                # optional, M9
ATMOSPHERIC_POST_SERVER_PROFILE=1 \                            # optional, M10
ATMOSPHERIC_FETCH_LIST="at://did:…/app.bsky.graph.list/<rkey>" \ # optional, M11
ATMOSPHERIC_FETCH_LABELS="at://did:…/app.bsky.feed.post/<rkey>" \ # optional, M12
ATMOSPHERIC_LABELER="did:plc:…" \                  # optional, M12 (labeler query)
ATMOSPHERIC_LABELER_SUBJECT="did:plc:…" \          # optional, M12 (default: own DID)
ATMOSPHERIC_FETCH_PLAYER="did:plc:… | handle" \     # optional, M13 (presence read)
ATMOSPHERIC_TRUSTED_SERVER_KEYS="keyId=publicKeyB64,…" \ # optional, M13
ATMOSPHERIC_FETCH_SKIN="did:plc:… | handle" \       # optional, M14 (skin read)
ATMOSPHERIC_GATE="at://…/app.bsky.graph.list/<rkey> | URL" \ # optional, M15
ATMOSPHERIC_GATE_SUBJECT="did:plc:… | handle" \     # optional, M15 (default: own DID)
ATMOSPHERIC_FETCH_CHAT="did:plc:… | handle" \       # optional, M16 (chat history)
ATMOSPHERIC_FETCH_SERVER="did:plc:… | handle" \     # optional, M19 (server profile)
ATMOSPHERIC_GRAPH="follows:did:plc:…" \            # optional, M20 (follows|followers|blocks|mutes)
ATMOSPHERIC_LIKE="at://…/app.bsky.feed.post/<rkey>" \    # optional, M21
ATMOSPHERIC_REPOST="at://…/app.bsky.feed.post/<rkey>" \  # optional, M21
ATMOSPHERIC_REPLY="at://…/app.bsky.feed.post/<rkey>|<text>" \ # optional, M21
cargo run -p atmospheric --example pds_check
```

OAuth (M8) instead of the app password — finish the login in the browser that
opens (set `ATMOSPHERIC_NO_BROWSER=1` on a headless machine: the authorize URL
is printed via the `OAuthAuthorize` event instead); a second run resumes the
persisted session without a browser:

```sh
ATMOSPHERIC_OAUTH_HANDLE="you.example" \
ATMOSPHERIC_POST_TEXT="hello over OAuth" \                    # optional
cargo run -p atmospheric --example pds_check
```

Headless wire check of the OAuth flow up to the browser step (resolve →
discover → PAR against the real services):

```sh
ATMOSPHERIC_OAUTH_HANDLE="you.example" ATMOSPHERIC_OAUTH_PAR_ONLY=1 \
cargo run -p atmospheric --example pds_check
```

Known third-party quirk: eurosky.social's authorize page answers headless
(non-browser) GETs with `400 Missing sec-fetch-site header` (bot guard) —
real browsers pass, and the PAR step itself is accepted by both
eurosky.social and bsky.social.

Credentials come from the environment only; the example writes them to the
temp settings file the worker reads and nothing else. Expected output: a
`Session(LoggedIn { did })` event, then `Posted { uri }`, then a clean
shutdown. The offline tests in `worker/service.rs` cover the same paths against a
stub PDS on loopback (`cargo test --workspace --locked --offline`).

## Build and validation

- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --locked` run on every push (`.github/workflows/ci.yml`).
- Native staging: `cargo fetch --locked`, then `cargo test --workspace --locked
  --offline`, matching the template's standalone checks. Full native checks
  need the pinned Horizon export from `compatibility.json`.
- The bundle pins the exact Horizon and horizon-mixins revisions; other
  revisions have not been validated.

### Native staging and the in-game check (done for M2)

With clones of the two upstream repositories beside this one, at the pinned
commits:

```sh
git -C ../horizon checkout 9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0   # pinned Horizon
git -C ../horizon-mixins checkout d9c83d7                            # pinned mixins
cargo build --locked -p horizon-mixins-cli --manifest-path ../horizon-mixins/Cargo.toml
mkdir -p target/horizon-export
git -C ../horizon archive 9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0 | tar -x -C target/horizon-export
find target/horizon-export -type l -delete   # the tool refuses links; nothing needs them
echo -n 9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0 > target/horizon-export/.horizon-revision
../horizon-mixins/target/debug/horizon-mixins plan  --workspace target/horizon-export --mods mixins --out target/mixins/plan.json
../horizon-mixins/target/debug/horizon-mixins apply --workspace target/horizon-export --mods mixins --out target/modded
cd target/modded && cargo build --locked --profile fast --no-default-features -p horizon-client
```

(`target/` is gitignored; the upstream checkouts are never modified.)

Then run the modded client with prepared 26.3 resources and a control
socket, in a scratch game home so a dev install is untouched:

```sh
HORIZON_HOME=/tmp/atmos-home target/modded/target/fast/horizon-client \
  --resources "<launcher>/versions/<nightly>/.local/horizon-client/26.3" \
  --control-socket /tmp/atmos-test.sock --world AtmosTest --render-distance 8
horizon-ctl --socket /tmp/atmos-test.sock state
```

Observed for M2 on the then-pinned revision: `plan`/`apply` succeed (1 module,
5 named-hook injections); the stage builds with no warnings; the game launches
and plays. In-game, verified through the control socket: Options > Mods lists
"Atmospheric 0.1.0" and toggles ON/OFF; typing `/atmospheric login` and
`/atmospheric post <text>` in chat shows the mod's chat feedback, so the
`command` hook fires and the commands go down the channel; control-socket key
events reach `native_key` (the keybind hook path) with no panics in the log;
and a pixel diff of screenshots with the mod OFF vs ON shows the HUD status
box in the bottom-left corner only when ON — the `hud` hook rendering the
`status_line()` fed by the `tick` hook's `drain_events()`. Since M17 the
worker side attaches in-game as a sidecar: copy the release
`atmospheric-worker` binary next to the `horizon-client` executable (or set
`ATMOSPHERIC_WORKER_BIN`), and the `tick` hook's `ensure_attached()` spawns it
on the first tick — `/atmospheric login` then reaches the worker over stdio
and the HUD shows the session events. Without the binary, sends are no-ops
and the worker is proven standalone by the crate tests, the `sidecar` test,
and the `pds_check` example.

Re-validated for #8 on `9f42064` (nightly 2026-11-13): with the staged client
running, `horizon-ctl command "/atmospheric"` replied `{"ok":true}` with the
mod's usage text in `chat`, proving the control socket takes the same client
`command` hook as typed chat. The `atmospheric-worker` sidecar attached with
`--data-dir`, and the chat-box key-event workaround from the older pin is no
longer needed. Upstream horizon-rust/horizon#9394 carries the routing fix.

### Releases (M18)

`CHANGELOG.md` is the human-readable release history; releases are annotated
tags (`v0.1.0`, `v0.2.0`, `v0.3.0`, `v0.3.1`, `v0.4.0`). A release is cut from `main`:

```sh
git tag -a v0.4.0 -m "atmospheric-horizon 0.4.0"
git push origin v0.4.0
```

`.github/workflows/release.yml` then (tag `v*` only):

1. checks the tag matches `version` in the root `Cargo.toml` (the same
   version `runtime.rs` pins to `CARGO_PKG_VERSION` in its unit test), builds
   `atmospheric-worker` with `--locked --release` for Linux x86_64, macOS
   arm64 and Windows x86_64, and packs each binary as
   `atmospheric-worker-vX.Y.Z-<platform>.tar.gz` (zip on Windows);
2. packs `atmospheric-horizon-mixins-vX.Y.Z.tar.gz` from `mixins/atmospheric`
   + `compatibility.json` + `LICENSE` + `README.md` + `CHANGELOG.md` + `docs/`
   — the bundle is source, staged by horizon-mixins against the pinned
   revision, not a prebuilt game binary;
3. writes `SHA256SUMS` and publishes the GitHub Release with generated notes
   and all artifacts.

The worker archives are the reason the release exists: the game spawns the
binary next to its executable (M17), so players need a per-OS build before the
registry entry (#28) can point at one. Release bumps are mechanical: change
the version in the root `Cargo.toml`, `crates/atmospheric/Cargo.toml`,
`mixins/atmospheric/horizon-mixin.toml` and `runtime.rs`'s `VERSION`, refresh
`Cargo.lock`, let the consistency test prove the library and runtime agree,
then tag.

## Reviewer notes

- Network use is declared, not hidden: this README, the mod page, and the
  first-run UX all say what the mod talks to and what it sends.
- No Minecraft assets, code, or data are embedded; nothing is read from the
  player's files beyond the mod's own storage folder.
- Licence: AGPL-3.0-only, matching this repository's `LICENSE`.
