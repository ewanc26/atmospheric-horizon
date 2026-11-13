//! AT Protocol client logic for the atmospheric Horizon mod.
//!
//! Everything in this crate builds and tests without a game: no Horizon
//! imports, no mixin declarations, only the protocol side. The game side (the
//! injected `runtime` module, kept std-only) talks to this crate through the
//! command/event channels owned by [`worker`], which is the only place that
//! performs network or disk I/O.

pub mod actions;
pub mod chats;
pub mod colibri;
pub mod config;
pub mod error;
pub mod graph;
pub mod interactions;
pub mod labels;
pub mod lists;
pub mod notifications;
pub mod oauth;
pub mod presence;
pub mod profiles;
pub mod records;
pub mod servers;
pub mod session;
pub mod signing;
pub mod skins;
pub mod tid;
pub mod worker;

pub use config::Config;
pub use error::AtmosphericError;
use serde::{Deserialize, Serialize};

/// What the game side asks the worker thread to do.
// Serde: the sidecar worker binary (M17) reads/writes these as newline-
// delimited JSON over stdio, externally tagged (the serde default):
// `"Login"`, `{"PostText":{"text":"…"}}` — the game-side module hand-rolls
// exactly this shape without serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Log in with the configured credentials (app-password flow).
    Login,
    /// Log in with localhost OAuth (M8): resolve the handle, open the browser
    /// at the authorization server, and exchange the loopback redirect.
    LoginOAuth { handle: String },
    /// Log in a separate server account with localhost OAuth (M25): the
    /// account atpcraft uses for its labeler, Colibri service auth, RPG
    /// attestations and server-side gate. Its session is persisted apart from
    /// the player session.
    LoginServerOAuth { handle: String },
    /// Report the server account's session state (M25).
    ServerAccountStatus,
    /// Drop the server account session and delete its persisted refresh token
    /// (M25).
    LogoutServerAccount,
    /// Replace the settings file with these settings.
    SaveConfig { config: Box<Config> },
    /// Post plain text to Bluesky.
    PostText { text: String },
    /// Upload an image file and post it as an embedded image.
    PostImage {
        path: String,
        alt_text: String,
        text: Option<String>,
    },
    /// Poll notifications now (the worker also does this on a timer).
    RefreshNotifications,
    /// Publish a chat message as an `at.atpcraft.player.chat` record
    /// (atpcraft port). `automatic` marks hook-originated messages, which the
    /// worker only publishes when `post_chat_messages` is enabled; explicit
    /// ones (the `/atmospheric chat` command) always publish.
    PostChat { message: String, automatic: bool },
    /// Start the Colibri bridge (M26) as the server account.
    ColibriStart,
    /// Reconnect the Colibri event stream while keeping the bridge active
    /// (M31). The worker re-mints the service-auth token; membership and
    /// profile state are unchanged.
    ColibriReconnect,
    /// Stop the Colibri bridge and drop its stream.
    ColibriStop,
    /// Report whether the bridge is active and connected.
    ColibriStatus,
    /// Publish one message into the bridged channel as the player (M26).
    /// `automatic` marks the chat-hook mirror; a missing bridge is silent for
    /// those, while the explicit command reports the failure.
    ColibriSend { text: String, automatic: bool },
    /// Relay one server broadcast line into the bridged channel as the server
    /// account (M28). The hook has already classified it; the worker silently
    /// ignores it when the bridge or server notices are off.
    ColibriNotice { text: String },
    /// Mark a server line as handled by the Colibri presence fallback (M29):
    /// `kind` is `join` or `leave`, or `None` for a blank/hidden line.
    ColibriHandled { kind: Option<String> },
    /// A join or leave connection event for the presence fallback (M29). The
    /// game side has already localized `text`; the worker waits 3 seconds and
    /// writes it only when no matching broadcast line arrived.
    ColibriPresence { kind: String, text: String },
    /// Publish the player's state as an `at.atpcraft.player.snapshot` record,
    /// endorsed with the mod's own Ed25519 key (atpcraft port, M6). An empty
    /// `summary.username` is filled from the configured identifier.
    PostSnapshot { summary: records::SnapshotSummary },
    /// Publish the player's skin (and optional cape) as an
    /// `at.atpcraft.player.skin` record, endorsed with the mod's own Ed25519
    /// key and upserted at the key id (atpcraft port, M9). `None` paths fall
    /// back to the configured `skin_path`/`cape_path`; `model` is
    /// "default" or "slim".
    PostSkin {
        skin_path: Option<String>,
        cape_path: Option<String>,
        model: String,
    },
    /// Publish/refresh the account's server profile
    /// (`at.atpcraft.server.profile` at rkey `self`, atpcraft port, M10).
    /// Identity fields come from the settings file; the signing key block is
    /// the mod's persisted Ed25519 key.
    PostServerProfile,
    /// Publish/refresh the server profile from the separate server account
    /// (M25), the atpcraft ownership model, instead of the player account.
    PostServerProfileAsServer,
    /// Fetch the member DIDs of a public Bluesky list — the read side of
    /// atpcraft's join gate (M11). The reference is an
    /// `at://…/app.bsky.graph.list/<rkey>` URI or a bsky.app list URL.
    FetchList { reference: String },
    /// The join-gate decision (M15): is `subject` (a DID or handle) on the
    /// list `list` (an AT-URI or bsky.app list URL)? Fail-closed like
    /// atpcraft's gate: any error is reported as a deny-with-reason.
    /// Unauthenticated AppView read — works logged out.
    CheckGate { list: String, subject: String },
    /// Read a player's `at.atpcraft.player.chat` records (their mirrored
    /// in-game chat history) from their own PDS (M16, the read side of the
    /// M5 chat mirror). Chat records are unsigned by design, so there is no
    /// endorsement to verify. Unauthenticated — works logged out.
    FetchChat { subject: String },
    /// Read the labels on a subject (M12, labeler read side): an AT-URI, a
    /// DID, a handle, or a bsky.app profile/post/list URL. Labels come from
    /// the subject's AppView view (`labels` field). Works logged out.
    FetchLabels { subject: String },
    /// Query a labeler directly (M12, labeler read side): resolve the
    /// labeler's `#atproto_labeler` DID-doc service endpoint and page
    /// `com.atproto.label.queryLabels` for `subject` (an AT-URI/DID/handle;
    /// `None` means the logged-in account's own DID). Works logged out when
    /// `subject` is given.
    FetchLabelerLabels {
        labeler: String,
        subject: Option<String>,
    },
    /// Read a player's latest `at.atpcraft.player.snapshot` record from their
    /// own PDS and verify its endorsement (M13, atpcraft presence read side).
    /// The subject is a DID or a handle. Works logged out: the read is an
    /// unauthenticated `com.atproto.repo.listRecords` against the subject's
    /// PDS.
    FetchPlayer { subject: String },
    /// Read an account's Bluesky profile (M24, atpcraft profile pronouns):
    /// `app.bsky.actor.getProfile` from the AppView plus the direct
    /// `app.bsky.actor.profile` record at rkey `self` from the account's own
    /// PDS. The direct record's `pronouns` wins when the read succeeds, even
    /// when the field is absent; a failed read falls back to the AppView.
    /// The subject is a DID or a handle. Unauthenticated — works logged out.
    FetchProfile { subject: String },
    /// Read a player's latest `at.atpcraft.player.skin` record from their own
    /// PDS, verify its endorsement, and — only when it verifies — download
    /// the skin/cape blobs into the mod's storage folder (M14, the atpcraft
    /// skin-sync read side). The subject is a DID or a handle; the reads are
    /// unauthenticated, so this works logged out.
    FetchPlayerSkin { subject: String },
    /// Read another account's `at.atpcraft.server.profile` record from their
    /// own PDS and verify its signing key against trusted keys (M19, the
    /// read side of M10). The subject is a DID or a handle; the read is
    /// unauthenticated — works logged out.
    FetchServerProfile { subject: String },
    /// Read one of an account's public social graphs (M20): `kind` is
    /// "follows", "followers", "blocks" or "mutes", and `subject` is a DID
    /// or a handle. Unauthenticated AppView read — works logged out.
    FetchGraph { kind: String, subject: String },
    /// Write one relationship (M23): `action` is "follow", "unfollow",
    /// "block", "unblock", "mute" or "unmute", and `subject` is a DID or a
    /// handle. Requires a session.
    GraphWrite { action: String, subject: String },
    /// Reply to a post (M21): publish an `app.bsky.feed.post` whose
    /// `reply` root/parent strong refs point at the target thread. The
    /// target is an `at://…/app.bsky.feed.post/<rkey>` URI or a bsky.app
    /// post URL; its CID is read unauthenticated from the author's PDS.
    Reply { uri: String, text: String },
    /// Like a post (M21): publish an `app.bsky.feed.like` strong-ref record.
    Like { uri: String },
    /// Repost a post (M21): publish an `app.bsky.feed.repost` strong-ref
    /// record.
    Repost { uri: String },
    /// Post the player's death message to Bluesky (only when
    /// `post_death_messages` is enabled).
    PostDeathMessage { text: String },
    /// Log out: drop the session and delete the persisted refresh token.
    Logout,
    /// Shut the worker thread down.
    Shutdown,
}

/// What the worker thread reports back to the game side.
// Serde: same sidecar wire format as [`Command`] (M17).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// A free-form HUD status line (M29 uses it for the pending-approval
    /// notice after a new Colibri membership is created).
    Status(String),
    /// Session health, mirrored on the HUD.
    Session(session::SessionState),
    /// The separate server account's session (M25). `handle` and `did` are
    /// present whenever the account is known, including while logging in.
    ServerAccount {
        state: session::SessionState,
        handle: Option<String>,
        did: Option<String>,
    },
    /// A record was written; carries its `at://` URI.
    Posted { uri: String },
    /// Something failed; carries a HUD-friendly message.
    Failed { message: String },
    /// The OAuth authorize URL to open in a browser (M8); the worker also
    /// tries to open it itself.
    OAuthAuthorize { url: String },
    /// The latest notification digest for the HUD panel.
    Notifications {
        items: Vec<notifications::NotificationDigest>,
    },
    /// The members of a fetched Bluesky list (M11): the AT-URI and the
    /// member DIDs, in page order.
    ListMembers { list: String, members: Vec<String> },
    /// The join-gate decision (M15): the resolved list AT-URI, the subject's
    /// DID, and whether the DID is on the list.
    GateDecision {
        list: String,
        subject: String,
        allowed: bool,
    },
    /// A player's chat history (M16): the DID (`subject`), the PDS it was
    /// read from (`via`), the total parseable record count, and the newest
    /// few messages (oldest first). Chat records carry no endorsement, so
    /// there is no verdict — they are plain records in the author's repo.
    ChatHistory {
        subject: String,
        via: String,
        total: usize,
        items: Vec<chats::ChatDigest>,
    },
    /// The labels found on a subject (M12, labeler read side): the subject
    /// (AT-URI or DID), where the labels came from (`via`: the AppView URL or
    /// the labeler's endpoint), and the labels themselves.
    Labels {
        subject: String,
        via: String,
        labels: Vec<labels::LabelDigest>,
    },
    /// The latest snapshot record of a player (M13, presence read side): the
    /// DID (`subject`), the PDS it was read from (`via`), and the digest with
    /// its endorsement verdict (`None` when the repo has no snapshot
    /// records).
    PlayerSnapshot {
        subject: String,
        via: String,
        // Boxed: PlayerDigest is the enum's largest variant.
        snapshot: Box<Option<presence::PlayerDigest>>,
    },
    /// An account's profile (M24, atpcraft profile pronouns): the resolved DID
    /// (`subject`), the PDS the direct record was read from (`via`), and the
    /// digest. The digest combines the AppView profile with the direct record,
    /// with atpcraft's pronoun precedence already applied.
    Profile {
        subject: String,
        via: String,
        profile: Box<profiles::ProfileDigest>,
    },
    /// The latest skin record of a player (M14, skin-sync read side): the DID
    /// (`subject`), the PDS it was read from (`via`), and the digest with its
    /// endorsement verdict and any downloaded blobs (`None` when the repo has
    /// no skin records).
    PlayerSkin {
        subject: String,
        via: String,
        // Boxed: SkinDigest is one of the event enum's largest variants.
        skin: Box<Option<skins::SkinDigest>>,
    },
    /// The latest server profile of an account (M19, the read side of M10):
    /// the DID (`subject`), the PDS it was read from (`via`), and the digest
    /// with its signing-key trust verdict (`None` when the repo has no
    /// server profile).
    ServerProfile {
        subject: String,
        via: String,
        // Boxed: ServerDigest is one of the event enum's largest variants.
        profile: Box<Option<servers::ServerDigest>>,
    },
    /// One of an account's social graphs (M20): the relation (`kind`), the
    /// subject's resolved DID, the AppView it was read from (`via`), and the
    /// accounts in page order.
    SocialGraph {
        kind: String,
        subject: String,
        via: String,
        members: Vec<graph::GraphMember>,
    },
    /// A relationship was written (M23): the action (`follow`, `unfollow`,
    /// `block`, `unblock`, `mute` or `unmute`) and the subject's resolved DID.
    GraphChanged { action: String, subject: String },
    /// Colibri bridge state (M26/M34). `error` is set when the stream dropped;
    /// the worker reconnects, so it is a status line rather than a terminal
    /// error. `community` is the owning community's URI.
    Colibri {
        active: bool,
        connected: bool,
        channel: String,
        community: String,
        error: Option<String>,
    },
    /// One Colibri message for the game chat (M26).
    ColibriMessage { handle: String, text: String },
    /// The local player has no `social.colibri.actor.profile` record (M30);
    /// the runtime shows atpcraft's setup prompt with this URL in chat.
    ColibriProfileMissing { url: String },
    /// A new local-player Colibri membership needs moderator approval (M32);
    /// the runtime shows `message` in chat and the HUD.
    ColibriMembershipPending { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sidecar wire format (M17) is serde's externally tagged default; the
    /// game-side module hand-rolls exactly these shapes, so pin them here.
    #[test]
    fn command_wire_shapes() {
        assert_eq!(
            serde_json::to_string(&Command::Login).unwrap(),
            r#""Login""#
        );
        assert_eq!(
            serde_json::to_string(&Command::PostText {
                text: "hi \"there\"\n".into()
            })
            .unwrap(),
            r#"{"PostText":{"text":"hi \"there\"\n"}}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::PostImage {
                path: "/tmp/a.png".into(),
                alt_text: "alt".into(),
                text: None,
            })
            .unwrap(),
            r#"{"PostImage":{"path":"/tmp/a.png","alt_text":"alt","text":null}}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::PostChat {
                message: "m".into(),
                automatic: true,
            })
            .unwrap(),
            r#"{"PostChat":{"message":"m","automatic":true}}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::FetchLabelerLabels {
                labeler: "did:plc:x".into(),
                subject: None,
            })
            .unwrap(),
            r#"{"FetchLabelerLabels":{"labeler":"did:plc:x","subject":null}}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::ColibriReconnect).unwrap(),
            r#""ColibriReconnect""#
        );
        assert_eq!(
            serde_json::to_string(&Command::ColibriHandled {
                kind: Some("join".into())
            })
            .unwrap(),
            r#"{"ColibriHandled":{"kind":"join"}}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::ColibriPresence {
                kind: "join".into(),
                text: "Alex joined the game".into(),
            })
            .unwrap(),
            r#"{"ColibriPresence":{"kind":"join","text":"Alex joined the game"}}"#
        );
        // SaveConfig carries the full Config; the game side sends the Settings
        // subset and container-level serde(default) fills the rest.
        let json = serde_json::to_string(&Command::SaveConfig {
            config: Box::new(Config {
                pds_url: "https://pds.example".into(),
                identifier: "alice.test".into(),
                app_password: "aaaa-bbbb-cccc-dddd".into(),
                ..Config::default()
            }),
        })
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["SaveConfig"]["config"]["pds_url"],
            "https://pds.example"
        );
        // Round-trip every variant's shape.
        let commands = vec![
            Command::Login,
            Command::LoginOAuth {
                handle: "a.test".into(),
            },
            Command::SaveConfig {
                config: Box::new(Config::default()),
            },
            Command::PostText { text: "t".into() },
            Command::PostImage {
                path: "p".into(),
                alt_text: "a".into(),
                text: Some("t".into()),
            },
            Command::RefreshNotifications,
            Command::PostChat {
                message: "m".into(),
                automatic: false,
            },
            Command::PostSnapshot {
                summary: records::SnapshotSummary {
                    username: "u".into(),
                    dimension: "overworld".into(),
                    position: records::Position { x: 1, y: 2, z: 3 },
                    health: "20".into(),
                    food: 20,
                    xp_level: 0,
                    game_mode: "survival".into(),
                    team: None,
                    scores: None,
                },
            },
            Command::PostSkin {
                skin_path: None,
                cape_path: Some("c".into()),
                model: "slim".into(),
            },
            Command::PostServerProfile,
            Command::FetchList {
                reference: "at://x/app.bsky.graph.list/y".into(),
            },
            Command::CheckGate {
                list: "l".into(),
                subject: "s".into(),
            },
            Command::FetchChat {
                subject: "did:plc:z".into(),
            },
            Command::FetchLabels {
                subject: "did:plc:z".into(),
            },
            Command::FetchLabelerLabels {
                labeler: "l".into(),
                subject: Some("s".into()),
            },
            Command::FetchPlayer {
                subject: "did:plc:z".into(),
            },
            Command::FetchPlayerSkin {
                subject: "did:plc:z".into(),
            },
            Command::FetchServerProfile {
                subject: "did:plc:z".into(),
            },
            Command::FetchGraph {
                kind: "follows".into(),
                subject: "did:plc:z".into(),
            },
            Command::GraphWrite {
                action: "follow".into(),
                subject: "did:plc:z".into(),
            },
            Command::Reply {
                uri: "at://did:plc:z/app.bsky.feed.post/1".into(),
                text: "nice".into(),
            },
            Command::Like {
                uri: "at://did:plc:z/app.bsky.feed.post/1".into(),
            },
            Command::Repost {
                uri: "at://did:plc:z/app.bsky.feed.post/1".into(),
            },
            Command::PostDeathMessage { text: "t".into() },
            Command::Logout,
            Command::Shutdown,
        ];
        for command in commands {
            let line = serde_json::to_string(&command).unwrap();
            assert!(!line.contains('\n'));
            let back: Command = serde_json::from_str(&line).unwrap();
            assert_eq!(format!("{back:?}"), format!("{command:?}"));
        }
    }

    #[test]
    fn event_wire_shapes() {
        assert_eq!(
            serde_json::to_string(&Event::Status("pending approval".into())).unwrap(),
            r#"{"Status":"pending approval"}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::ColibriProfileMissing {
                url: "https://colibri.social".into()
            })
            .unwrap(),
            r#"{"ColibriProfileMissing":{"url":"https://colibri.social"}}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::ColibriMembershipPending {
                message: "Your join is pending. A moderator must approve it.".into()
            })
            .unwrap(),
            r#"{"ColibriMembershipPending":{"message":"Your join is pending. A moderator must approve it."}}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::Session(session::SessionState::LoggedOut)).unwrap(),
            r#"{"Session":"LoggedOut"}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::Session(session::SessionState::LoggedIn {
                did: "did:plc:x".into()
            }))
            .unwrap(),
            r#"{"Session":{"LoggedIn":{"did":"did:plc:x"}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::Posted {
                uri: "at://did:plc:x/app.bsky.feed.post/3k".into()
            })
            .unwrap(),
            r#"{"Posted":{"uri":"at://did:plc:x/app.bsky.feed.post/3k"}}"#
        );
        // Box<Option<_>> serializes transparently: null or the inner object.
        assert_eq!(
            serde_json::to_string(&Event::PlayerSnapshot {
                subject: "did:plc:x".into(),
                via: "https://pds.example".into(),
                snapshot: Box::new(None),
            })
            .unwrap(),
            r#"{"PlayerSnapshot":{"subject":"did:plc:x","via":"https://pds.example","snapshot":null}}"#
        );
        let events = vec![
            Event::Session(session::SessionState::Failed {
                message: "m".into(),
            }),
            Event::Posted { uri: "u".into() },
            Event::Failed {
                message: "m".into(),
            },
            Event::OAuthAuthorize {
                url: "https://x".into(),
            },
            Event::Notifications { items: vec![] },
            Event::ListMembers {
                list: "l".into(),
                members: vec!["did:plc:a".into()],
            },
            Event::GateDecision {
                list: "l".into(),
                subject: "s".into(),
                allowed: true,
            },
            Event::ChatHistory {
                subject: "s".into(),
                via: "v".into(),
                total: 2,
                items: vec![chats::ChatDigest {
                    rkey: "r".into(),
                    at_uri: "at://u".into(),
                    sent_at: "2026-01-01T00:00:00.000Z".into(),
                    server: "srv".into(),
                    message: "hi".into(),
                }],
            },
            Event::Labels {
                subject: "s".into(),
                via: "v".into(),
                labels: vec![],
            },
            Event::PlayerSnapshot {
                subject: "s".into(),
                via: "v".into(),
                snapshot: Box::new(None),
            },
            Event::PlayerSkin {
                subject: "s".into(),
                via: "v".into(),
                skin: Box::new(None),
            },
            Event::ServerProfile {
                subject: "s".into(),
                via: "v".into(),
                profile: Box::new(None),
            },
            Event::SocialGraph {
                kind: "follows".into(),
                subject: "s".into(),
                via: "v".into(),
                members: vec![graph::GraphMember {
                    did: "did:plc:a".into(),
                    handle: "a.test".into(),
                    display_name: "A".into(),
                }],
            },
            Event::GraphChanged {
                action: "follow".into(),
                subject: "did:plc:a".into(),
            },
            Event::ColibriProfileMissing {
                url: "https://colibri.social".into(),
            },
            Event::ColibriMembershipPending {
                message: "Your join is pending. A moderator must approve it.".into(),
            },
        ];
        for event in events {
            let line = serde_json::to_string(&event).unwrap();
            assert!(!line.contains('\n'));
            let back: Event = serde_json::from_str(&line).unwrap();
            assert_eq!(format!("{back:?}"), format!("{event:?}"));
        }
    }
}
