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

- `install()` creates the `GameCommand`/`ModEvent` channel pair, keeps the
  game side in statics, and hands the worker side to whoever spawns the
  worker at startup.
- Hook helpers push `GameCommand`s through the sender — never blocking,
  never panicking if the worker is gone. M2 hooks Horizon's named native
  hook points (`native-hooks.json` at the pinned revision): `mods` (the
  Options > Mods row and its keybind), `key` (the keybind posts the
  player's location), `command` (`/atmospheric login`,
  `/atmospheric post <text>`), `tick` (drains events once per tick on the
  window's game) and `hud` (draws the status line in the bottom-left
  corner).
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
- `worker` — owns the tokio runtime, the agent, and the files; the **only**
  module that performs network or disk I/O
- `error` — one error enum, mapped to HUD strings at the edge

### The M2 wiring, and why the worker is not linked into the game yet

The injected module lives inside the game's crate, which does not depend on
the `atmospheric` crate. The worker therefore consumes the game-side channels
and owns an `atmospheric`-crate agent inside its own thread — the two crates
never import each other, they meet only at the channel boundary, and commands
cross it as plain data (`GameCommand`/`ModEvent` mirror `Command`/`Event`).

horizon-mixins 0.6.0 can stage a bundle's own crate into the game workspace
(`[[crate]]`), but its registry dependencies are taken only from Horizon's
existing `Cargo.lock` — the allow list — and the pinned Horizon revision's
lock has no `atrium-api`/`reqwest` (it does have `serde`, `tokio`,
`thiserror`). So the bundle stays mixins-only for now: the game-side hooks
above are real and compiled in, but no in-game consumer is attached to the
channel yet. When Horizon's lock (or the launcher) allows it, a small glue
helper calls `install()`, spawns `atmospheric::worker::spawn` with
`native_data_dir("atmospheric")` as the storage folder, and maps
`Command`↔`GameCommand`; the boundary stays exactly as it is.

Until then, M2's end-to-end proof runs the worker standalone against a real
PDS — see "Manual check" below.

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
  the loop can later `select!` a notification timer, M4).

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
  token is persisted again. `Command::Logout` (`/atmospheric logout`) drops
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
the loopback exception need a hosted client-metadata URL instead — ship
`oauth-client-metadata.json` (GitHub Pages etc.) and set `oauth_client_id`
in `config.json`.

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
`status_line()` fed by the `tick` hook's `drain_events()`. The worker side of
the channel has no in-game consumer yet (see above), so those sends are no-ops
until the glue lands; the worker itself is proven by the crate tests and the
`pds_check` example.

## Reviewer notes

- Network use is declared, not hidden: this README, the mod page, and the
  first-run UX all say what the mod talks to and what it sends.
- No Minecraft assets, code, or data are embedded; nothing is read from the
  player's files beyond the mod's own storage folder.
- Licence: AGPL-3.0-only, matching this repository's `LICENSE`.
