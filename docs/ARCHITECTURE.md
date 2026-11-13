# Architecture

How atmospheric-horizon is put together, for reviewers, contributors, and coding agents. Start with `AGENTS.md` for the working rules.

## The two layers

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
- Hook helpers push `GameCommand`s through the sender — never blocking,
  never panicking if the worker is gone. M2 hooks Horizon's named native
  hook points (`native-hooks.json` at the pinned revision): `mods` (the
  Options > Mods row and its keybind), `key` (the keybind posts the
  player's location), `command` (`/atmospheric login`,
  `/atmospheric post <text>`), `tick` (attaches the sidecar once, then drains
  events once per tick on the window's game) and `hud` (draws the status
  line in the bottom-left corner).
- `drain_events()` is called once per tick to collect worker events, folding
  each into the HUD status line (`status_line()`), bounded so the game
  thread never stalls.

### The protocol library: `crates/atmospheric`

Everything else, with zero Horizon imports so it builds and tests with plain
`cargo test`:

- `config` — settings, stored under the mod's own folder
- `session` — `AtpAgent` construction and app-password login
- `actions` — `create_record` posts (text, and image posts with an
  `app.bsky.embed.images` embed after `uploadBlob`); MIME sniffing and PNG
  dimensions are pure helpers here
- `notifications` — polling with a persisted seen-cursor (dedup across
  restarts, the pattern bsky-bot-sdk uses)
- `lists` — M11 public list reads (`app.bsky.graph.getList` pagination) and
  the M15 join-gate decision (`check_gate`)
- `graph` — M20 social-graph reads (`app.bsky.graph.getFollows`/
  `getFollowers`/`getBlocks`/`getMutes` pagination on the public AppView,
  reduced to DID/handle/display name)
- `chats` — M16 chat-history reads (`at.atpcraft.player.chat` listRecords on
  the subject's own PDS, ordered by `sentAt`)
- `servers` — M19 server-profile reads (`at.atpcraft.server.profile` listRecords
  on the subject's own PDS, preferred `self` rkey, `signingKey` trust
  verification against your own key and `trusted_server_keys`)
- `labels` — M12 labeler read side (AppView view `labels`, labeler
  `#atproto_labeler` endpoint resolution, `com.atproto.label.queryLabels`)
- `presence` — M13 verified reads of other players' `at.atpcraft.player.snapshot`
  records (`com.atproto.repo.listRecords` on the subject's own PDS, highest
  version, Ed25519 endorsement verification)
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

- `records.rs` reimplements atpcraft's `at.atpcraft.player.chat` lexicon as a
  plain `ChatRecord { sentAt, message, server }` (message truncated to the
  lexicon's 1024 cap). atpcraft's repo carries no licence, so no code is
  copied — only the wire format is matched, which is the point of a lexicon.
  `tid.rs` generates the TID record keys atpcraft uses (53-bit microsecond
  timestamp + 10-bit clock id, sortable base32, 13 chars, strictly monotonic
  per worker), so a player's chat records accumulate as a time-sorted,
  append-only history.
- `actions::post_chat` writes one record per message via `createRecord` with
  the TID as `rkey`; the worker owns the single `TidGen`. `Command::PostChat
  { message, automatic }`: `automatic` (chat-hook origin) publishes only when
  `post_chat_messages` is enabled in `config.json` (default off — chat is
  public once written); `automatic: false` (the `/atmospheric chat <text>`
  command) always publishes. The `server` field comes from
  `config.chat_server_id` (default `horizon-client`).
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

- `records.rs` adds the `defs` shapes (`Position`, `Score`, `TeamSummary`,
  `SnapshotSummary`), `Aspect`, `Endorsement`, and `SnapshotRecord`, plus
  `canonical_json` — the deterministic JSON (recursively sorted keys, only
  `\"`/`\\` escaped) that atpcraft signs. `SnapshotRecord::signable_payload`
  builds the exact payload atpcraft's `signablePayload` builds:
  `{did, version, issuedAt, server, summary, aspects[]}` (aspects name-sorted
  as `{name, blob: cid}`).
- `signing.rs` is the Ed25519 endorsement key (pure-Rust `ed25519-dalek`,
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
lives in `crates/atmospheric/src/oauth.rs` and follows the atproto OAuth
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
JWK in `session.json`) signs every request. `session.rs` has a custom
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
version), the logged-in DID/handle as `operator`, and the mod's persisted
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
endpoint. Label issuance stays server-side (a client mod cannot host the
labeler HTTP endpoint).

- `labels.rs` parses a subject reference — an `at://` URI (account or
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
  `labels::LabelDigest { val, src, neg, cts, exp }` reduced from
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
`presence.rs`:

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
atpcraft's `BlueskyJoinGate` membership decision as a client-side read
(enforcement itself stays server-side). `lists::check_gate` resolves the
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
shutdown. The offline tests in `worker.rs` cover the same paths against a
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
git -C ../horizon checkout ba6344a59cb9e755b730dcf02285fc0d33b97d3c   # pinned Horizon
git -C ../horizon-mixins checkout d9c83d7                            # pinned mixins
cargo build --locked -p horizon-mixins-cli --manifest-path ../horizon-mixins/Cargo.toml
mkdir -p target/horizon-export
git -C ../horizon archive ba6344a59cb9e755b730dcf02285fc0d33b97d3c | tar -x -C target/horizon-export
find target/horizon-export -type l -delete   # the tool refuses links; nothing needs them
echo -n ba6344a59cb9e755b730dcf02285fc0d33b97d3c > target/horizon-export/.horizon-revision
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

Observed for M2 on the pinned revision: `plan`/`apply` succeed (1 module, 5
named-hook injections); the stage builds with no warnings; the game launches
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

### Releases (M18)

`CHANGELOG.md` is the human-readable release history; the first tag is
`v0.1.0`. A release is cut from `main`:

```sh
git tag -a v0.1.0 -m "atmospheric-horizon 0.1.0"
git push origin v0.1.0
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
the version in both `Cargo.toml`s and `runtime.rs`'s `VERSION`, let the
consistency test prove they agree, then tag.

## Reviewer notes

- Network use is declared, not hidden: this README, the mod page, and the
  first-run UX all say what the mod talks to and what it sends.
- No Minecraft assets, code, or data are embedded; nothing is read from the
  player's files beyond the mod's own storage folder.
- Licence: AGPL-3.0-only, matching this repository's `LICENSE`.
