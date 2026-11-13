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
- Label issuance and server-side join-gate enforcement are server concerns
  and are documented, not implemented in the client mod (#29).

[Unreleased]: https://github.com/ewanc26/atmospheric-horizon/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.2.0
[0.1.0]: https://github.com/ewanc26/atmospheric-horizon/releases/tag/v0.1.0
