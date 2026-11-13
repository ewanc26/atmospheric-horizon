// Runtime module for the atmospheric native mod.
//
// This exact source is also injected as a same-crate module by horizon-mixins
// (as `crate::game::atmospheric`), so it must stay std-only: no external
// crates, no game imports. All AT Protocol work happens off-thread in the
// `atmospheric` crate (crates/atmospheric), reached through the channels
// installed here; commands and events cross that boundary as plain data only.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

/// The mod's id: its settings folder and its keys in Options > Mods.
pub const ID: &str = "atmospheric";
/// The mod's name and version, as Options > Mods lists them.
pub const NAME: &str = "Atmospheric";
pub const VERSION: &str = "0.2.0";
/// The mod's description on its Options > Mods row.
pub const DESCRIPTION: &str = "Post to Bluesky from Horizon: a keybind posts your location, /atmospheric posts text or logs in. Talks only to the PDS you configure.";
/// The keybind's name, label and default key, as Options > Mods lists them.
pub const KEY_POST: &str = "post_location";
pub const KEY_POST_LABEL: &str = "Post location to Bluesky";
pub const KEY_POST_DEFAULT: &str = "key.keyboard.b";
/// The second keybind: post the latest F2 screenshot.
pub const KEY_SHOT: &str = "post_screenshot";
pub const KEY_SHOT_LABEL: &str = "Post latest screenshot to Bluesky";
pub const KEY_SHOT_DEFAULT: &str = "key.keyboard.v";
/// The third keybind: publish a player-state snapshot record (M6).
pub const KEY_SNAPSHOT: &str = "post_snapshot";
pub const KEY_SNAPSHOT_LABEL: &str = "Publish player snapshot record";
pub const KEY_SNAPSHOT_DEFAULT: &str = "key.keyboard.c";

/// The HUD status box, drawn as the WebAssembly mods' panels were: translucent
/// black with white shadowed text.
pub const BACKGROUND: u32 = 0x7800_0000;
pub const INK: u32 = 0xFF_FFFF;

/// Plain settings, carried across the channel; the worker side maps them
/// onto its `Config`. Credentials are app passwords, never account passwords.
// Read by the worker glue once the bundle crate can be staged into the game
// build; until then the game-side hooks compile against these boundary types.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub pds_url: String,
    pub identifier: String,
    pub app_password: String,
    pub poll_interval_secs: u64,
    pub post_death_messages: bool,
    pub post_chat_messages: bool,
    pub chat_server_id: String,
}

/// Commands the game side sends to the worker thread.
// The worker side is not linked into the game build yet (see
// docs/ARCHITECTURE.md), so only `Login`/`PostText` are constructed today.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum GameCommand {
    /// Log in with the configured credentials (app-password flow).
    Login,
    /// Log in with localhost OAuth (M8): the worker resolves the handle, opens
    /// the browser at the authorization server, and completes the loopback
    /// redirect.
    LoginOAuth { handle: String },
    /// Log in the separate server account with localhost OAuth (M25).
    LoginServerOAuth { handle: String },
    /// Report the server account's session state (M25).
    ServerAccountStatus,
    /// Drop the server account session and delete its persisted token (M25).
    LogoutServerAccount,
    /// Replace the settings file with these settings.
    SaveConfig { settings: Settings },
    /// Post plain text to Bluesky.
    PostText { text: String },
    /// Upload an image file and post it as an embedded image (M3).
    PostImage {
        path: String,
        alt_text: String,
        text: Option<String>,
    },
    /// Poll notifications now (the worker also does this on a timer, M4).
    RefreshNotifications,
    /// Publish a chat message as an `at.atpcraft.player.chat` record (M5,
    /// atpcraft port). `automatic` marks chat-hook messages, which the worker
    /// only publishes when `post_chat_messages` is enabled.
    PostChat { message: String, automatic: bool },
    /// Start the Colibri bridge as the server account (M26).
    ColibriStart,
    /// Reconnect the Colibri event stream with a fresh token (M31).
    ColibriReconnect,
    /// Stop the Colibri bridge (M26).
    ColibriStop,
    /// Report the Colibri bridge state (M26).
    ColibriStatus,
    /// Publish a Colibri message as the player (M26). `automatic` is the
    /// chat-hook mirror, which stays silent when the bridge is off.
    ColibriSend { text: String, automatic: bool },
    /// Relay a server broadcast line as the server account (M28).
    ColibriNotice { text: String },
    /// Mark a line as handled for the presence fallback (M29). `kind` is
    /// `join` or `leave`, or `None` for a blank/hidden line.
    ColibriHandled { kind: Option<String> },
    /// A join/leave connection event for the presence fallback (M29).
    ColibriPresence { kind: String, text: String },
    /// Publish the player's state as an `at.atpcraft.player.snapshot` record
    /// (M6, atpcraft port).
    PostSnapshot { summary: SnapshotSummary },
    /// Publish the player's skin (and optional cape) as an
    /// `at.atpcraft.player.skin` record (M9, atpcraft port). `None` paths fall
    /// back to the settings file's `skin_path`/`cape_path`.
    PostSkin {
        skin_path: Option<String>,
        cape_path: Option<String>,
        model: String,
    },
    /// Publish/refresh the account's server profile record
    /// (`at.atpcraft.server.profile` at rkey `self`, M9-era atpcraft port).
    PostServerProfile,
    /// Publish/refresh the server profile from the separate server account
    /// (M25), as atpcraft's server account owns it.
    PostServerProfileAsServer,
    /// Fetch a public Bluesky list's member DIDs (M11, join-gate read side):
    /// an `at://…app.bsky.graph.list/<rkey>` URI or a bsky.app list URL.
    FetchList { reference: String },
    /// The join-gate decision (M15): is `subject` (a DID or handle) on the
    /// list `list` (an AT-URI or bsky.app list URL)? Fail-closed — errors
    /// come back as `ModEvent::Failed` (the deny-with-reason).
    CheckGate { list: String, subject: String },
    /// Read a player's `at.atpcraft.player.chat` records (their mirrored
    /// in-game chat history) from their own PDS (M16, read side of the M5
    /// chat mirror). Unsigned records by design — no endorsement to verify.
    /// Works logged out.
    FetchChat { subject: String },
    /// Read the labels on a subject (M12, labeler read side): an AT-URI, a
    /// DID, a handle, or a bsky.app profile/post/list URL.
    FetchLabels { subject: String },
    /// Query a labeler's `com.atproto.label.queryLabels` for a subject (M12):
    /// the labeler's `#atproto_labeler` endpoint is resolved from its DID
    /// document; `None` subject means the logged-in account's own DID.
    FetchLabelerLabels {
        labeler: String,
        subject: Option<String>,
    },
    /// Read a player's latest `at.atpcraft.player.snapshot` record from their
    /// own PDS and verify its endorsement (M13, atpcraft presence read side):
    /// the subject is a DID or a handle. Works logged out.
    FetchPlayer { subject: String },
    /// Read an account's Bluesky profile (M24): `app.bsky.actor.getProfile`
    /// from the AppView plus the direct `app.bsky.actor.profile` record from
    /// the account's own PDS, with atpcraft's pronoun precedence. Works
    /// logged out.
    FetchProfile { subject: String },
    /// Read a player's latest `at.atpcraft.player.skin` record from their own
    /// PDS, verify its endorsement, and — only when verified — download the
    /// skin/cape blobs into the mod's storage folder (M14, atpcraft skin-sync
    /// read side). Works logged out.
    FetchPlayerSkin { subject: String },
    /// Read an account's `at.atpcraft.server.profile` record from their own
    /// PDS and verify its signing key against trusted keys (M19, read side
    /// of M10). Works logged out.
    FetchServerProfile { subject: String },
    /// Read one of an account's public social graphs (M20): `kind` is
    /// "follows", "followers", "blocks" or "mutes", and `subject` is a DID
    /// or a handle. Works logged out.
    FetchGraph { kind: String, subject: String },
    /// Write one relationship (M23): `action` is "follow", "unfollow",
    /// "block", "unblock", "mute" or "unmute"; `subject` is a DID or handle.
    GraphWrite { action: String, subject: String },
    /// Reply to a post (M21): publish an `app.bsky.feed.post` with
    /// `reply: { root, parent }` strong refs after reading the target's CID.
    Reply { uri: String, text: String },
    /// Like a post (M21): publish an `app.bsky.feed.like` strong-ref record.
    Like { uri: String },
    /// Repost a post (M21): publish an `app.bsky.feed.repost` strong-ref record.
    Repost { uri: String },
    /// Post the player's death message (only when `post_death_messages` is on).
    PostDeathMessage { text: String },
    /// Log out: drop the session and delete the persisted refresh token.
    Logout,
    /// Stop the worker thread.
    Shutdown,
}

/// One chat record, reduced to what the HUD shows (M16). Plain data: the
/// worker side has the same struct in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDigest {
    pub rkey: String,
    pub at_uri: String,
    pub sent_at: String,
    pub server: String,
    /// The message, truncated for the HUD line.
    pub message: String,
}

/// One account in a social-graph read (M20), reduced to what the HUD shows.
/// Plain data: the worker side has the same struct in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphMember {
    pub did: String,
    pub handle: String,
    pub display_name: String,
}

/// The player-state summary a snapshot record carries. Plain data; the
/// worker side maps it onto the `at.atpcraft.player.defs#summary` shape.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSummary {
    pub username: String,
    pub dimension: String,
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub health: String,
    pub food: i64,
    pub xp_level: i64,
    pub game_mode: String,
}

/// Build a summary from the player the game reports. Fields Horizon's
/// `native_player` does not expose (health, food, xp, game mode) get sane
/// defaults; an empty username is filled from the configured identifier by
/// the worker.
pub fn snapshot_from_player(
    username: &str,
    dimension: &str,
    x: f64,
    y: f64,
    z: f64,
) -> SnapshotSummary {
    SnapshotSummary {
        username: username.into(),
        dimension: dimension.into(),
        x: x.floor() as i64,
        y: y.floor() as i64,
        z: z.floor() as i64,
        health: "20".into(),
        food: 20,
        xp_level: 0,
        game_mode: "survival".into(),
    }
}

/// Session health, mirrored on the HUD. Plain data: the worker side has the
/// same enum with protocol types behind it.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    LoggedOut,
    LoggingIn,
    LoggedIn { did: String },
    Failed { message: String },
}

/// One notification, reduced to what the HUD shows. Plain data: the worker
/// side has the same struct with protocol types behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDigest {
    pub uri: String,
    pub author_handle: String,
    pub reason: String,
    pub text_snippet: String,
}

/// One label on a subject, reduced to what the HUD shows (M12). Plain data:
/// the worker side maps `com.atproto.label.defs#label` onto this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelDigest {
    pub val: String,
    pub src: String,
    pub neg: bool,
    pub cts: String,
    pub exp: Option<String>,
}

/// The verdict of verifying a record's endorsements (M13/M14). Plain data:
/// the worker side has the same enum in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndorsementVerdict {
    /// At least one endorsement from a known key verified.
    Verified { endorser: String },
    /// No endorsement came from a known key.
    UnknownEndorser,
    /// Known keys were present but every signature failed.
    BadSignature,
    /// The record carries no endorsements.
    Malformed,
}

/// A player's latest snapshot record, reduced to what the HUD shows (M13).
/// Plain data: the worker side has the same struct in the `atmospheric`
/// crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerDigest {
    pub did: String,
    pub rkey: String,
    pub at_uri: String,
    pub version: i64,
    pub issued_at: String,
    pub username: String,
    pub dimension: String,
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub health: String,
    pub food: i64,
    pub xp_level: i64,
    pub game_mode: String,
    pub verdict: EndorsementVerdict,
}

/// An account's Bluesky profile, reduced to what the HUD shows (M24). Plain
/// data: the worker side has the same struct in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDigest {
    pub did: String,
    pub handle: String,
    pub display_name: String,
    pub description: String,
    pub pronouns: Option<String>,
    pub avatar: String,
}

/// One referenced or downloaded skin/cape blob, reduced to what the HUD
/// shows (M14). Plain data: the worker side has the same struct in the
/// `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobDigest {
    pub cid: String,
    pub mime_type: String,
    pub size: u64,
    /// Where the worker saved the bytes; `None` until downloaded (the worker
    /// only downloads verified records).
    pub path: Option<String>,
}

/// A player's latest skin record, reduced to what the HUD shows (M14). Plain
/// data: the worker side has the same struct in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinDigest {
    pub did: String,
    pub rkey: String,
    pub at_uri: String,
    pub model: String,
    pub timestamp: String,
    pub verdict: EndorsementVerdict,
    pub skin: Option<BlobDigest>,
    pub cape: Option<BlobDigest>,
}

/// The trust verdict on a server profile's `signingKey` block (M19). Plain
/// data: the worker side has the same enum in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerVerdict {
    /// `key_id` is a key you trust (own key or a `trusted_server_keys` entry)
    /// and its published public key matches.
    Trusted { key_id: String },
    /// `key_id` is not in the trusted set.
    Unknown,
    /// `key_id` is trusted but its `publicKey` does not match the key you have.
    KeyMismatch { key_id: String },
    /// The record is not a structurally valid server profile.
    Malformed,
}

/// What the HUD shows for a server profile (M19). Plain data: the worker side
/// has the same struct in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDigest {
    pub did: String,
    pub rkey: String,
    pub at_uri: String,
    pub name: String,
    pub description: String,
    pub address: String,
    pub minecraft_version: String,
    pub atpcraft_version: String,
    pub capabilities: Vec<String>,
    pub operator_handle: String,
    pub created_at: Option<String>,
    pub updated_at: String,
    pub signing_key_id: String,
    pub verdict: ServerVerdict,
}

/// Events the worker thread reports back to the game side.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModEvent {
    /// A free-form status line.
    Status(String),
    /// Session health.
    Session(SessionState),
    /// The separate server account's session (M25): its state plus the
    /// handle/DID the worker knows.
    ServerAccount {
        state: SessionState,
        handle: Option<String>,
        did: Option<String>,
    },
    /// A record was written; carries its `at://` URI.
    Posted { uri: String },
    /// Something failed; carries a HUD-friendly message.
    Failed { message: String },
    /// The OAuth authorize URL to open in a browser (M8).
    OAuthAuthorize { url: String },
    /// The latest notification digest (M4).
    Notifications { items: Vec<NotificationDigest> },
    /// The members of a fetched Bluesky list (M11, join-gate read side).
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
    /// few messages (oldest first). Unsigned records by design.
    ChatHistory {
        subject: String,
        via: String,
        total: usize,
        items: Vec<ChatDigest>,
    },
    /// The labels found on a subject (M12, labeler read side): the subject
    /// (AT-URI or DID), where the labels came from (`via`: the AppView URL or
    /// the labeler's endpoint), and the labels themselves.
    Labels {
        subject: String,
        via: String,
        labels: Vec<LabelDigest>,
    },
    /// The latest snapshot record of a player (M13, presence read side): the
    /// DID (`subject`), the PDS it was read from (`via`), and the digest with
    /// its endorsement verdict (`None` when the repo has no snapshot records).
    PlayerSnapshot {
        subject: String,
        via: String,
        // Boxed: PlayerDigest is the enum's largest variant.
        snapshot: Box<Option<PlayerDigest>>,
    },
    /// An account's profile (M24): the resolved DID (`subject`), the PDS the
    /// direct record was read from (`via`), and the combined digest with
    /// atpcraft's pronoun precedence already applied.
    Profile {
        subject: String,
        via: String,
        profile: Box<ProfileDigest>,
    },
    /// The latest skin record of a player (M14, skin-sync read side): the DID
    /// (`subject`), the PDS it was read from (`via`), and the digest with its
    /// endorsement verdict and any downloaded blobs (`None` when the repo has
    /// no skin records).
    PlayerSkin {
        subject: String,
        via: String,
        // Boxed: SkinDigest is one of the event enum's largest variants.
        skin: Box<Option<SkinDigest>>,
    },
    /// The latest server profile of an account (M19, the read side of M10):
    /// the DID (`subject`), the PDS it was read from (`via`), and the digest
    /// with its signing-key trust verdict (`None` when the repo has no
    /// server profile).
    ServerProfile {
        subject: String,
        via: String,
        // Boxed: ServerDigest is one of the event enum's largest variants.
        profile: Box<Option<ServerDigest>>,
    },
    /// One of an account's social graphs (M20): the relation (`kind`), the
    /// subject's resolved DID, the AppView it was read from (`via`), and the
    /// accounts in page order.
    SocialGraph {
        kind: String,
        subject: String,
        via: String,
        members: Vec<GraphMember>,
    },
    /// A relationship was written (M23): the action and the resolved DID.
    GraphChanged { action: String, subject: String },
    /// Colibri bridge state (M26).
    Colibri {
        active: bool,
        connected: bool,
        channel: String,
        error: Option<String>,
    },
    /// One incoming Colibri message (M26).
    ColibriMessage { handle: String, text: String },
    /// The local player has no Colibri actor profile (M30); `url` is the
    /// setup page shown in chat.
    ColibriProfileMissing { url: String },
}

/// The HUD wording for an endorsement verdict (M13/M14).
fn verdict_text(verdict: &EndorsementVerdict) -> String {
    match verdict {
        EndorsementVerdict::Verified { endorser } => format!("verified by {endorser}"),
        EndorsementVerdict::UnknownEndorser => "unknown endorser".into(),
        EndorsementVerdict::BadSignature => "BAD SIGNATURE".into(),
        EndorsementVerdict::Malformed => "malformed record".into(),
    }
}

/// The HUD line one event leaves behind.
pub fn status_text(event: &ModEvent) -> String {
    match event {
        ModEvent::Status(text) => text.clone(),
        ModEvent::Session(SessionState::LoggedOut) => "atmospheric: logged out".into(),
        ModEvent::Session(SessionState::LoggingIn) => "atmospheric: logging in…".into(),
        ModEvent::Session(SessionState::LoggedIn { did }) => {
            format!("atmospheric: logged in as {did}")
        }
        ModEvent::Session(SessionState::Failed { message }) => {
            format!("atmospheric: login failed: {message}")
        }
        ModEvent::ServerAccount { state, handle, did } => match state {
            SessionState::LoggedOut => "atmospheric: server account logged out".into(),
            SessionState::LoggingIn => "atmospheric: logging in the server account…".into(),
            SessionState::LoggedIn { .. } => {
                let account = handle
                    .as_ref()
                    .map(|h| format!("@{h}"))
                    .or_else(|| did.clone())
                    .unwrap_or_else(|| "server account".into());
                format!("atmospheric: server account {account} logged in")
            }
            SessionState::Failed { message } => {
                format!("atmospheric: server account login failed: {message}")
            }
        },
        ModEvent::Posted { uri } => format!("atmospheric: posted {uri}"),
        ModEvent::Failed { message } => format!("atmospheric: {message}"),
        ModEvent::OAuthAuthorize { url } => format!("atmospheric: finish login at {url}"),
        ModEvent::ListMembers { list, members } => {
            format!("atmospheric: list {list} has {} members", members.len())
        }
        ModEvent::GateDecision {
            list,
            subject,
            allowed,
        } => format!(
            "atmospheric: gate {list} — {subject} {}",
            if *allowed { "ALLOWED" } else { "DENIED" }
        ),
        ModEvent::ChatHistory {
            subject,
            total,
            items,
            ..
        } => {
            if *total == 0 {
                format!("atmospheric: no chat records for {subject}")
            } else {
                let latest = items
                    .last()
                    .map(|i| format!(" — latest from {}: \"{}\"", i.server, i.message))
                    .unwrap_or_default();
                format!("atmospheric: {total} chat record(s) for {subject} (unsigned){latest}")
            }
        }
        ModEvent::Labels {
            subject,
            via,
            labels,
        } => {
            if labels.is_empty() {
                format!("atmospheric: no labels on {subject}")
            } else {
                let heads: Vec<String> = labels
                    .iter()
                    .take(3)
                    .map(|l| {
                        if l.neg {
                            format!("-{}", l.val)
                        } else {
                            l.val.clone()
                        }
                    })
                    .collect();
                let more = if labels.len() > 3 {
                    format!(" +{} more", labels.len() - 3)
                } else {
                    String::new()
                };
                format!(
                    "atmospheric: {} label(s) on {subject} via {via}: {}{more}",
                    labels.len(),
                    heads.join(", ")
                )
            }
        }
        ModEvent::PlayerSnapshot {
            subject, snapshot, ..
        } => match &**snapshot {
            None => format!("atmospheric: no snapshot records for {subject}"),
            Some(d) => {
                let verdict = verdict_text(&d.verdict);
                format!(
                    "atmospheric: {} v{} in {} at {}, {}, {} — {verdict}",
                    d.username, d.version, d.dimension, d.x, d.y, d.z
                )
            }
        },
        ModEvent::Profile {
            subject, profile, ..
        } => {
            let name = if profile.display_name.is_empty() {
                if profile.handle.is_empty() {
                    subject.clone()
                } else {
                    profile.handle.clone()
                }
            } else {
                profile.display_name.clone()
            };
            let mut line = format!("atmospheric: {name}");
            if !profile.handle.is_empty() && profile.handle != name {
                line.push_str(&format!(" (@{})", profile.handle));
            }
            if let Some(pronouns) = &profile.pronouns {
                line.push_str(&format!(" ({pronouns})"));
            }
            if !profile.description.is_empty() {
                let mut description: String = profile.description.chars().take(80).collect();
                if profile.description.chars().count() > 80 {
                    description.push('…');
                }
                line.push_str(&format!(" — {description}"));
            }
            line
        }
        ModEvent::PlayerSkin { subject, skin, .. } => match &**skin {
            None => format!("atmospheric: no skin records for {subject}"),
            Some(d) => {
                let verdict = verdict_text(&d.verdict);
                let blob = |b: &Option<BlobDigest>| match b {
                    None => "none".to_string(),
                    Some(b) => {
                        let short = &b.cid[..b.cid.len().min(12)];
                        match &b.path {
                            Some(path) => format!("{short}… saved to {path}"),
                            None => format!("{short}… {} B, not saved", b.size),
                        }
                    }
                };
                format!(
                    "atmospheric: {subject} skin model {} — skin {}, cape {} — {verdict}",
                    d.model,
                    blob(&d.skin),
                    blob(&d.cape)
                )
            }
        },
        ModEvent::ServerProfile {
            subject, profile, ..
        } => match &**profile {
            None => format!("atmospheric: no server profile for {subject}"),
            Some(d) => {
                let verdict = match &d.verdict {
                    ServerVerdict::Trusted { key_id } => format!("key {key_id} trusted"),
                    ServerVerdict::Unknown => {
                        if d.signing_key_id.is_empty() {
                            "key unknown".to_string()
                        } else {
                            format!("key {} unknown", d.signing_key_id)
                        }
                    }
                    ServerVerdict::KeyMismatch { key_id } => format!("key {key_id} mismatched"),
                    ServerVerdict::Malformed => "malformed record".into(),
                };
                let n_caps = d.capabilities.len();
                let caps = if n_caps == 1 {
                    "1 capability".to_string()
                } else {
                    format!("{n_caps} capabilities")
                };
                format!(
                    "atmospheric: {} — MC {}, {caps}, {verdict}",
                    d.name, d.minecraft_version
                )
            }
        },
        ModEvent::GraphChanged { action, subject } => {
            let past = match action.as_str() {
                "follow" => "followed",
                "unfollow" => "unfollowed",
                "block" => "blocked",
                "unblock" => "unblocked",
                "mute" => "muted",
                "unmute" => "unmuted",
                _ => "updated",
            };
            format!("atmospheric: {past} {subject}")
        }
        ModEvent::Colibri {
            active,
            connected,
            channel,
            error,
        } => {
            if !active {
                "atmospheric: Colibri bridge off".into()
            } else if let Some(error) = error {
                format!("atmospheric: Colibri disconnected: {error}")
            } else if *connected {
                format!("atmospheric: Colibri connected to {channel}")
            } else {
                format!("atmospheric: Colibri connecting to {channel}")
            }
        }
        ModEvent::ColibriMessage { handle, text } => {
            format!(
                "[Colibri] @{}: {}",
                clean_remote(handle),
                clean_remote(text)
            )
        }
        ModEvent::ColibriProfileMissing { url } => {
            format!(
                "atmospheric: Colibri profile missing — open {}",
                clean_remote(url)
            )
        }
        ModEvent::SocialGraph {
            kind,
            subject,
            via,
            members,
        } => {
            if members.is_empty() {
                format!("atmospheric: no {kind} for {subject} (via {via})")
            } else {
                let heads: Vec<String> = members
                    .iter()
                    .take(3)
                    .map(|m| {
                        if m.handle.is_empty() {
                            m.did.clone()
                        } else {
                            m.handle.clone()
                        }
                    })
                    .collect();
                let more = if members.len() > 3 {
                    format!(" +{} more", members.len() - 3)
                } else {
                    String::new()
                };
                format!(
                    "atmospheric: {kind} of {subject} via {via}: {} account(s) — {}{more}",
                    members.len(),
                    heads.join(", ")
                )
            }
        }
        ModEvent::Notifications { items } => {
            if items.is_empty() {
                "atmospheric: no new notifications".into()
            } else {
                let heads: Vec<String> = items
                    .iter()
                    .take(3)
                    .map(|i| format!("{} from {}", i.reason, i.author_handle))
                    .collect();
                format!("atmospheric: {} new: {}", items.len(), heads.join(", "))
            }
        }
    }
}

/// The line the keybind posts: the player's position, as `/where` writes it.
pub fn location_line(x: f64, y: f64, z: f64, dimension: &str) -> String {
    format!(
        "At {:.0}, {:.0}, {:.0} in {dimension} — from Horizon",
        x.floor(),
        y.floor(),
        z.floor()
    )
}

static COMMAND_TX: OnceLock<Sender<GameCommand>> = OnceLock::new();
static EVENTS_RX: OnceLock<Mutex<Receiver<ModEvent>>> = OnceLock::new();
static STATUS: OnceLock<Mutex<Vec<(String, std::time::Instant)>>> = OnceLock::new();
static LAST_SHOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static COLIBRI_CHAT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

/// How many status lines the HUD shows at once.
pub const STATUS_LINES_MAX: usize = 3;

/// How long a HUD status line stays visible before it expires (#17).
pub const STATUS_LINE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn status_slot() -> &'static Mutex<Vec<(String, std::time::Instant)>> {
    // Empty until the first worker event: with no worker attached (no sidecar
    // binary found, see issue #2) there is nothing worth showing, so the HUD
    // box stays hidden.
    STATUS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Drop status lines older than [`STATUS_LINE_TTL`].
fn prune_expired(status: &mut Vec<(String, std::time::Instant)>, now: std::time::Instant) {
    status.retain(|(_, at)| now.duration_since(*at) < STATUS_LINE_TTL);
}

fn last_shot_slot() -> &'static Mutex<Option<String>> {
    LAST_SHOT.get_or_init(|| Mutex::new(None))
}

/// Create the channel pair and install the game side in-process.
///
/// Returns the worker side — the receiving end of the commands and the
/// sending end of the events — exactly once. A second call returns `None`.
/// In-game the worker attaches as a sidecar process instead (M17,
/// [`ensure_attached`]); this in-process pair is for tests and for a future
/// `[[crate]]` glue when horizon-mixins can stage the worker crate (issue #2
/// options 1/2).
#[allow(dead_code)]
pub fn install() -> Option<(Receiver<GameCommand>, Sender<ModEvent>)> {
    if COMMAND_TX.get().is_some() {
        return None;
    }
    let (command_tx, command_rx) = std::sync::mpsc::channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let _ = COMMAND_TX.set(command_tx);
    if EVENTS_RX.set(Mutex::new(event_rx)).is_err() {
        return None; // Already installed.
    }
    Some((command_rx, event_tx))
}

/// Drain pending worker events, oldest first, folding each into the HUD
/// status line.
///
/// The game calls this once per tick; each drain is bounded so the game
/// thread never stalls on the worker.
pub fn drain_events(max: usize) -> Vec<ModEvent> {
    let mut drained = Vec::new();
    if let Some(rx) = EVENTS_RX.get()
        && let Ok(rx) = rx.lock()
    {
        while drained.len() < max {
            match rx.try_recv() {
                Ok(event) => drained.push(event),
                Err(_) => break,
            }
        }
    }
    if let Ok(mut chat) = COLIBRI_CHAT.get_or_init(|| Mutex::new(Vec::new())).lock() {
        for event in &drained {
            chat.extend(colibri_chat_lines(event));
            while chat.len() > 8 {
                chat.remove(0);
            }
        }
    }
    if let Ok(mut status) = status_slot().lock() {
        let now = std::time::Instant::now();
        prune_expired(&mut status, now);
        for event in &drained {
            let line = status_text(event);
            // Keep a short history instead of only the latest event, so a
            // notification digest is not overwritten immediately (#7).
            if status.last().map(|(l, _)| l) != Some(&line) {
                status.push((line, now));
            }
            while status.len() > STATUS_LINES_MAX {
                status.remove(0);
            }
        }
    }
    drained
}

/// The current HUD status lines, oldest first; empty until the first worker
/// event, in which case the HUD draws nothing. Lines older than
/// [`STATUS_LINE_TTL`] have expired and are dropped (#17).
pub fn status_lines() -> Vec<String> {
    status_slot()
        .lock()
        .map(|mut status| {
            prune_expired(&mut status, std::time::Instant::now());
            status.iter().map(|(line, _)| line.clone()).collect()
        })
        .unwrap_or_default()
}

/// The newest status line (empty when there is none).
pub fn status_line() -> String {
    status_lines().pop().unwrap_or_default()
}

/// The native-chat lines one Colibri event contributes. Incoming messages and
/// the M30 profile prompt share the same queue, so the prompt appears in chat
/// exactly where atpcraft sends it.
fn colibri_chat_lines(event: &ModEvent) -> Vec<String> {
    match event {
        ModEvent::ColibriMessage { handle, text } => vec![format!(
            "[Colibri] @{}: {}",
            clean_remote(handle),
            clean_remote(text)
        )],
        ModEvent::ColibriProfileMissing { url } => vec![
            "[Colibri] You need a Colibri profile to use the chat bridge.".into(),
            format!("[Colibri] Open Colibri: {}", clean_remote(url)),
        ],
        _ => Vec::new(),
    }
}

/// Take the incoming Colibri lines queued for native chat.
pub fn drain_colibri_chat() -> Vec<String> {
    COLIBRI_CHAT
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map(|mut lines| std::mem::take(&mut *lines))
        .unwrap_or_default()
}

/// Remote chat text is not trusted: drop control characters, the legacy
/// colour marker, and cap the line at the Colibri limit.
fn clean_remote(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() && *c != '§')
        .take(512)
        .collect()
}

/// Send a command to the worker, if installed. Never blocks, never panics
/// if the worker is gone.
fn send(command: GameCommand) {
    if let Some(tx) = COMMAND_TX.get() {
        let _ = tx.send(command);
    }
}

/// Ask the worker to log in with the configured credentials.
pub fn login() {
    send(GameCommand::Login);
}

/// Ask the worker to log in with localhost OAuth for this handle (M8). The
/// worker opens the system browser at the authorization server and completes
/// the loopback redirect.
pub fn login_oauth(handle: &str) {
    send(GameCommand::LoginOAuth {
        handle: handle.into(),
    });
}

/// Ask the worker to post these settings to the settings file.
#[allow(dead_code)]
pub fn save_config(settings: Settings) {
    send(GameCommand::SaveConfig { settings });
}

/// Ask the worker to post text to Bluesky.
pub fn post_text(text: &str) {
    send(GameCommand::PostText { text: text.into() });
}

/// Remember the path of a screenshot the game just saved (F2), so the
/// keybind can post that file. In-memory only: nothing is scanned or read.
pub fn remember_screenshot(path: &str) {
    if let Ok(mut slot) = last_shot_slot().lock() {
        *slot = Some(path.into());
    }
}

/// The latest screenshot's path, if the game saved one this session.
pub fn latest_screenshot() -> Option<String> {
    last_shot_slot().lock().ok().and_then(|slot| slot.clone())
}

/// Ask the worker to post the latest screenshot. Returns false when the game
/// has not saved one yet, so the caller can say so.
pub fn post_screenshot(alt_text: &str) -> bool {
    match latest_screenshot() {
        Some(path) => {
            send(GameCommand::PostImage {
                path,
                alt_text: alt_text.into(),
                text: None,
            });
            true
        }
        None => false,
    }
}

/// Ask the worker to poll notifications now (it also polls on a timer).
pub fn refresh_notifications() {
    send(GameCommand::RefreshNotifications);
}

/// Ask the worker to publish a chat message as an `at.atpcraft.player.chat`
/// record. `automatic` is true for messages the chat hook mirrored (published
/// only when `post_chat_messages` is enabled), false for the explicit
/// `/atmospheric chat` command.
pub fn post_chat(message: &str, automatic: bool) {
    send(GameCommand::PostChat {
        message: message.into(),
        automatic,
    });
    // atpcraft mirrors game chat into the bridged Colibri channel. The worker
    // ignores this silently while the bridge is off.
    send(GameCommand::ColibriSend {
        text: message.into(),
        automatic: true,
    });
}

/// Arm the Colibri bridge (M26).
pub fn colibri_start() {
    send(GameCommand::ColibriStart);
}

/// Reconnect the Colibri event stream with a fresh service token (M31).
pub fn colibri_reconnect() {
    send(GameCommand::ColibriReconnect);
}

/// Disarm the Colibri bridge (M26).
pub fn colibri_stop() {
    send(GameCommand::ColibriStop);
}

/// Ask for the bridge's current state (M26).
pub fn colibri_status() {
    send(GameCommand::ColibriStatus);
}

/// Publish one explicit message into the bridged channel (M26).
pub fn colibri_send(text: &str) {
    send(GameCommand::ColibriSend {
        text: text.into(),
        automatic: false,
    });
}

/// Relay one server broadcast line as the server account (M28).
pub fn post_colibri_notice(text: &str) {
    send(GameCommand::ColibriNotice { text: text.into() });
}

/// Mark a join/leave line, or a blank/hidden line, handled for the M29
/// presence fallback.
pub fn post_colibri_handled(kind: Option<&str>) {
    send(GameCommand::ColibriHandled {
        kind: kind.map(str::to_string),
    });
}

/// Schedule a join/leave fallback after a connection event (M29).
pub fn post_colibri_presence(kind: &str, text: &str) {
    send(GameCommand::ColibriPresence {
        kind: kind.into(),
        text: text.into(),
    });
}

/// The fallback kind for a server broadcast translation key (M29), or `None`
/// for ordinary chat and unknown lines.
pub fn colibri_notice_kind(key: &str) -> Option<&'static str> {
    match key {
        "multiplayer.player.joined" | "multiplayer.player.joined.renamed" => Some("join"),
        "multiplayer.player.left" => Some("leave"),
        "chat.type.advancement.task"
        | "chat.type.advancement.goal"
        | "chat.type.advancement.challenge" => Some("advancement"),
        _ if key.starts_with("death.") => Some("death"),
        _ => None,
    }
}

/// The server broadcast translation keys atpcraft relays. Ordinary chat,
/// command output and unknown server lines are deliberately excluded.
pub fn is_colibri_notice(key: &str) -> bool {
    colibri_notice_kind(key).is_some()
}

/// Ask the worker to publish a player-state snapshot record (M6).
pub fn post_snapshot(summary: SnapshotSummary) {
    send(GameCommand::PostSnapshot { summary });
}

/// Ask the worker to publish a skin record (M9). `None` paths mean "use the
/// settings file"; `model` is "default" or "slim".
pub fn post_skin(skin_path: Option<String>, cape_path: Option<String>, model: &str) {
    send(GameCommand::PostSkin {
        skin_path,
        cape_path,
        model: model.into(),
    });
}

/// Ask the worker to publish/refresh the server profile record (M10).
pub fn post_server_profile() {
    send(GameCommand::PostServerProfile);
}

/// Start OAuth login for the separate server account (M25).
pub fn login_server_oauth(handle: &str) {
    send(GameCommand::LoginServerOAuth {
        handle: handle.into(),
    });
}

/// Ask the worker to report the server account's session state (M25).
pub fn server_account_status() {
    send(GameCommand::ServerAccountStatus);
}

/// Ask the worker to drop the server-account session (M25).
pub fn logout_server_account() {
    send(GameCommand::LogoutServerAccount);
}

/// Ask the worker to publish the server profile from the server account (M25).
pub fn post_server_profile_as_server() {
    send(GameCommand::PostServerProfileAsServer);
}

/// Ask the worker to fetch a public Bluesky list's members (M11).
pub fn fetch_list(reference: &str) {
    send(GameCommand::FetchList {
        reference: reference.into(),
    });
}

/// Ask the worker for the join-gate decision (M15): is `subject` on `list`?
pub fn check_gate(list: &str, subject: &str) {
    send(GameCommand::CheckGate {
        list: list.into(),
        subject: subject.into(),
    });
}

/// Ask the worker to read a player's chat history (M16). `subject` is a DID
/// or a handle.
pub fn fetch_chat(subject: &str) {
    send(GameCommand::FetchChat {
        subject: subject.into(),
    });
}

/// Ask the worker to read the labels on a subject via its AppView view (M12).
pub fn fetch_labels(subject: &str) {
    send(GameCommand::FetchLabels {
        subject: subject.into(),
    });
}

/// Ask the worker to query a labeler for a subject's labels (M12). `subject`
/// `None` means the logged-in account's own DID.
pub fn fetch_labeler_labels(labeler: &str, subject: Option<&str>) {
    send(GameCommand::FetchLabelerLabels {
        labeler: labeler.into(),
        subject: subject.map(str::to_owned),
    });
}

/// Ask the worker to read a player's latest snapshot record and verify its
/// endorsement (M13). `subject` is a DID or a handle.
pub fn fetch_player(subject: &str) {
    send(GameCommand::FetchPlayer {
        subject: subject.into(),
    });
}

/// Ask the worker to read an account's Bluesky profile (M24). `subject` is a
/// DID or a handle.
pub fn fetch_profile(subject: &str) {
    send(GameCommand::FetchProfile {
        subject: subject.into(),
    });
}

/// Ask the worker to read a player's latest skin record, verify its
/// endorsement, and download the blobs when verified (M14). `subject` is a
/// DID or a handle.
pub fn fetch_player_skin(subject: &str) {
    send(GameCommand::FetchPlayerSkin {
        subject: subject.into(),
    });
}

/// Ask the worker to read an account's latest server profile record and verify
/// its published signing key (M19, read side of M10). `subject` is a DID or a
/// handle.
pub fn fetch_server_profile(subject: &str) {
    send(GameCommand::FetchServerProfile {
        subject: subject.into(),
    });
}

/// Ask the worker to read one of an account's public social graphs (M20):
/// `kind` is "follows", "followers", "blocks" or "mutes"; `subject` is a DID
/// or a handle.
pub fn fetch_graph(kind: &str, subject: &str) {
    send(GameCommand::FetchGraph {
        kind: kind.into(),
        subject: subject.into(),
    });
}

fn graph_write(action: &str, subject: &str) {
    send(GameCommand::GraphWrite {
        action: action.into(),
        subject: subject.into(),
    });
}

/// Ask the worker to follow an account (M23). `subject` is a DID or handle.
pub fn follow(subject: &str) {
    graph_write("follow", subject);
}

/// Ask the worker to undo a follow (M23).
pub fn unfollow(subject: &str) {
    graph_write("unfollow", subject);
}

/// Ask the worker to block an account (M23).
pub fn block(subject: &str) {
    graph_write("block", subject);
}

/// Ask the worker to undo a block (M23).
pub fn unblock(subject: &str) {
    graph_write("unblock", subject);
}

/// Ask the worker to mute an account (M23).
pub fn mute(subject: &str) {
    graph_write("mute", subject);
}

/// Ask the worker to undo a mute (M23).
pub fn unmute(subject: &str) {
    graph_write("unmute", subject);
}

/// Ask the worker to reply to a post (M21). `uri` is an
/// `at://…/app.bsky.feed.post/<rkey>` URI or a bsky.app post URL.
pub fn reply(uri: &str, text: &str) {
    send(GameCommand::Reply {
        uri: uri.into(),
        text: text.into(),
    });
}

/// Ask the worker to like a post (M21).
pub fn like(uri: &str) {
    send(GameCommand::Like { uri: uri.into() });
}

/// Ask the worker to repost a post (M21).
pub fn repost(uri: &str) {
    send(GameCommand::Repost { uri: uri.into() });
}

/// Ask the worker to post a death message (published only when the settings
/// enable `post_death_messages`).
pub fn post_death_message(text: &str) {
    send(GameCommand::PostDeathMessage { text: text.into() });
}

/// Ask the worker to log out (drops the session and its persisted token).
pub fn logout() {
    send(GameCommand::Logout);
}

/// Ask the worker to stop.
#[allow(dead_code)]
pub fn shutdown() {
    send(GameCommand::Shutdown);
}

// ---- M17: the sidecar worker bridge (issue #2 option 3, #40) ----
//
// The game crate cannot depend on the `atmospheric` crate (horizon-mixins
// allow-lists registry deps to Horizon's Cargo.lock), so instead of linking
// the worker in, the game spawns the `atmospheric-worker` binary next to the
// game executable and bridges the channel pair over its stdio: one JSON
// `GameCommand` line per command in, one JSON `ModEvent` line per event out.
// The wire format is serde's externally tagged default — pinned crate-side
// in `crates/atmospheric/src/lib.rs` tests; encoded/decoded here by hand
// because this module is std-only.

/// The sidecar binary's file name, looked up next to the game executable.
const WORKER_BIN_NAME: &str = if cfg!(windows) {
    "atmospheric-worker.exe"
} else {
    "atmospheric-worker"
};

/// The directory the game executable lives in.
fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())
}

/// Where the sidecar worker binary is and which data dir to give it:
/// `ATMOSPHERIC_WORKER_BIN` / `ATMOSPHERIC_DATA_DIR` override the defaults
/// (the binary next to the game executable, `<exe dir>/atmospheric-data`).
fn sidecar_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let bin = std::env::var_os("ATMOSPHERIC_WORKER_BIN")
        .map(std::path::PathBuf::from)
        .or_else(|| exe_dir().map(|dir| dir.join(WORKER_BIN_NAME)))
        .filter(|path| path.is_file());
    let data_dir = std::env::var_os("ATMOSPHERIC_DATA_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| exe_dir().map(|dir| dir.join("atmospheric-data")));
    bin.zip(data_dir)
}

static ATTACHED: OnceLock<bool> = OnceLock::new();

/// Attach a worker exactly once: spawn the sidecar binary if one is found
/// (M17), otherwise leave the channel unattached — sends stay no-ops and the
/// HUD stays hidden, exactly as before. Returns true when a worker is
/// attached (including an earlier in-process [`install`]). The tick helper
/// calls this; after the first call it is a cheap static read.
pub fn ensure_attached() -> bool {
    *ATTACHED.get_or_init(|| {
        if COMMAND_TX.get().is_some() {
            return true;
        }
        match sidecar_paths() {
            Some((bin, data_dir)) => attach_sidecar(&bin, &data_dir),
            None => false,
        }
    })
}

/// Spawn the sidecar worker binary and wire the channel pair to its stdio.
/// Returns the game side of the pair (commands out, events in), or `None`
/// when the binary could not be spawned. The caller owns the ends; when the
/// command sender is dropped, the writer thread closes the child's stdin and
/// the worker stops (and a reaper thread collects the child).
fn spawn_sidecar(
    bin: &std::path::Path,
    data_dir: &std::path::Path,
) -> Option<(Sender<GameCommand>, Receiver<ModEvent>)> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(bin)
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (command_tx, command_rx) = std::sync::mpsc::channel::<GameCommand>();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<ModEvent>();

    // Commands -> child stdin, one encoded JSON line each.
    std::thread::Builder::new()
        .name("atmospheric-sidecar-out".into())
        .spawn(move || {
            use std::io::Write;
            let mut stdin = stdin;
            while let Ok(command) = command_rx.recv() {
                let line = encode_command(&command);
                let written = stdin
                    .write_all(line.as_bytes())
                    .and_then(|_| stdin.write_all(b"\n"))
                    .and_then(|_| stdin.flush())
                    .is_ok();
                if !written || matches!(command, GameCommand::Shutdown) {
                    break;
                }
            }
            // `stdin` drops here: the worker sees EOF and stops.
        })
        .ok()?;

    // Child stdout -> events. Unreadable lines become a Failed event so the
    // HUD says something instead of silently dropping worker output.
    std::thread::Builder::new()
        .name("atmospheric-sidecar-in".into())
        .spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let event = decode_event(&line).unwrap_or_else(|| ModEvent::Failed {
                    message: format!("unreadable worker event: {}", &line[..line.len().min(120)]),
                });
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        })
        .ok()?;

    // Reap the child when it exits so it never lingers as a zombie.
    std::thread::Builder::new()
        .name("atmospheric-sidecar-wait".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .ok()?;

    Some((command_tx, event_rx))
}

/// Attach a sidecar worker to the channel statics (used by
/// [`ensure_attached`]). Returns true on success.
fn attach_sidecar(bin: &std::path::Path, data_dir: &std::path::Path) -> bool {
    let Some((command_tx, event_rx)) = spawn_sidecar(bin, data_dir) else {
        return false;
    };
    if COMMAND_TX.set(command_tx).is_err() || EVENTS_RX.set(Mutex::new(event_rx)).is_err() {
        return false;
    }
    true
}

// ---- Minimal JSON (std-only), just enough for the sidecar wire format ----

/// A parsed JSON value. Numbers are integers only — the wire format carries
/// no floats.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

fn jstr(s: &str) -> Json {
    Json::Str(s.to_string())
}
fn jint(n: i64) -> Json {
    Json::Int(i128::from(n))
}
fn juint(n: u64) -> Json {
    Json::Int(i128::from(n))
}
fn jbool(b: bool) -> Json {
    Json::Bool(b)
}
fn jopt_str(value: &Option<String>) -> Json {
    match value {
        Some(s) => Json::Str(s.clone()),
        None => Json::Null,
    }
}
fn jobj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
/// Serialize a JSON value (compact, like serde_json::to_string).
fn write_json(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Str(s) => write_json_str(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_str(k, out);
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

fn write_json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse one JSON document. Returns `None` on any syntax error.
fn parse_json(input: &str) -> Option<Json> {
    let mut p = Parser {
        chars: input.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos == p.chars.len() {
        Some(value)
    } else {
        None
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }
    fn expect(&mut self, c: char) -> Option<()> {
        if self.next() == Some(c) {
            Some(())
        } else {
            None
        }
    }
    fn value(&mut self) -> Option<Json> {
        self.skip_ws();
        match self.peek()? {
            'n' => self.literal("null", Json::Null),
            't' => self.literal("true", Json::Bool(true)),
            'f' => self.literal("false", Json::Bool(false)),
            '"' => self.string().map(Json::Str),
            '[' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.peek() == Some(']') {
                    self.pos += 1;
                    return Some(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.skip_ws();
                    match self.next()? {
                        ',' => continue,
                        ']' => return Some(Json::Arr(items)),
                        _ => return None,
                    }
                }
            }
            '{' => {
                self.pos += 1;
                let mut pairs = Vec::new();
                self.skip_ws();
                if self.peek() == Some('}') {
                    self.pos += 1;
                    return Some(Json::Obj(pairs));
                }
                loop {
                    self.skip_ws();
                    let key = self.string()?;
                    self.skip_ws();
                    self.expect(':')?;
                    let value = self.value()?;
                    pairs.push((key, value));
                    self.skip_ws();
                    match self.next()? {
                        ',' => continue,
                        '}' => return Some(Json::Obj(pairs)),
                        _ => return None,
                    }
                }
            }
            '-' | '0'..='9' => self.number(),
            _ => None,
        }
    }
    fn literal(&mut self, text: &str, value: Json) -> Option<Json> {
        for c in text.chars() {
            if self.next() != Some(c) {
                return None;
            }
        }
        Some(value)
    }
    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some('0'..='9')) {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<i128>().ok().map(Json::Int)
    }
    fn string(&mut self) -> Option<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.next()? {
                '"' => return Some(out),
                '\\' => match self.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hi = self.hex4()?;
                        let code = if (0xD800..0xDC00).contains(&hi) {
                            // High surrogate: a low surrogate must follow.
                            if self.next() != Some('\\') || self.next() != Some('u') {
                                return None;
                            }
                            let lo = self.hex4()?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return None;
                            }
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else {
                            hi
                        };
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                },
                c => out.push(c),
            }
        }
    }
    fn hex4(&mut self) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let c = self.next()?;
            let digit = c.to_digit(16)?;
            value = value * 16 + digit;
        }
        Some(value)
    }
}

// Decoding helpers: each returns None on a shape mismatch.
fn jget<'a>(obj: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
fn as_obj(value: &Json) -> Option<&[(String, Json)]> {
    match value {
        Json::Obj(pairs) => Some(pairs),
        _ => None,
    }
}
fn as_str(value: &Json) -> Option<&str> {
    match value {
        Json::Str(s) => Some(s),
        _ => None,
    }
}
fn as_int(value: &Json) -> Option<i128> {
    match value {
        Json::Int(n) => Some(*n),
        _ => None,
    }
}
fn as_bool(value: &Json) -> Option<bool> {
    match value {
        Json::Bool(b) => Some(*b),
        _ => None,
    }
}
fn as_arr(value: &Json) -> Option<&[Json]> {
    match value {
        Json::Arr(items) => Some(items),
        _ => None,
    }
}
/// `null` -> Some(None), string -> Some(Some), anything else -> None.
fn as_opt_str(value: &Json) -> Option<Option<String>> {
    match value {
        Json::Null => Some(None),
        Json::Str(s) => Some(Some(s.clone())),
        _ => None,
    }
}
fn req_str(obj: &[(String, Json)], key: &str) -> Option<String> {
    as_str(jget(obj, key)?).map(str::to_string)
}
fn req_int(obj: &[(String, Json)], key: &str) -> Option<i128> {
    as_int(jget(obj, key)?)
}

/// Encode a game-side command as one JSON line in the sidecar wire format
/// (serde's externally tagged default, as pinned in the crate's lib tests):
/// unit variants are bare strings, struct variants `{"Variant":{...}}`.
fn encode_command(command: &GameCommand) -> String {
    let value = match command {
        GameCommand::Login => jstr("Login"),
        GameCommand::LoginOAuth { handle } => {
            jobj(vec![("LoginOAuth", jobj(vec![("handle", jstr(handle))]))])
        }
        GameCommand::LoginServerOAuth { handle } => jobj(vec![(
            "LoginServerOAuth",
            jobj(vec![("handle", jstr(handle))]),
        )]),
        GameCommand::ServerAccountStatus => jstr("ServerAccountStatus"),
        GameCommand::LogoutServerAccount => jstr("LogoutServerAccount"),
        GameCommand::SaveConfig { settings } => jobj(vec![(
            "SaveConfig",
            jobj(vec![(
                "config",
                jobj(vec![
                    ("pds_url", jstr(&settings.pds_url)),
                    ("identifier", jstr(&settings.identifier)),
                    ("app_password", jstr(&settings.app_password)),
                    ("poll_interval_secs", juint(settings.poll_interval_secs)),
                    ("post_death_messages", jbool(settings.post_death_messages)),
                    ("post_chat_messages", jbool(settings.post_chat_messages)),
                    ("chat_server_id", jstr(&settings.chat_server_id)),
                ]),
            )]),
        )]),
        GameCommand::PostText { text } => {
            jobj(vec![("PostText", jobj(vec![("text", jstr(text))]))])
        }
        GameCommand::PostImage {
            path,
            alt_text,
            text,
        } => jobj(vec![(
            "PostImage",
            jobj(vec![
                ("path", jstr(path)),
                ("alt_text", jstr(alt_text)),
                ("text", jopt_str(text)),
            ]),
        )]),
        GameCommand::RefreshNotifications => jstr("RefreshNotifications"),
        GameCommand::PostChat { message, automatic } => jobj(vec![(
            "PostChat",
            jobj(vec![
                ("message", jstr(message)),
                ("automatic", jbool(*automatic)),
            ]),
        )]),
        GameCommand::ColibriStart => jstr("ColibriStart"),
        GameCommand::ColibriReconnect => jstr("ColibriReconnect"),
        GameCommand::ColibriStop => jstr("ColibriStop"),
        GameCommand::ColibriStatus => jstr("ColibriStatus"),
        GameCommand::ColibriSend { text, automatic } => jobj(vec![(
            "ColibriSend",
            jobj(vec![("text", jstr(text)), ("automatic", jbool(*automatic))]),
        )]),
        GameCommand::ColibriNotice { text } => {
            jobj(vec![("ColibriNotice", jobj(vec![("text", jstr(text))]))])
        }
        GameCommand::ColibriHandled { kind } => jobj(vec![(
            "ColibriHandled",
            jobj(vec![("kind", jopt_str(kind))]),
        )]),
        GameCommand::ColibriPresence { kind, text } => jobj(vec![(
            "ColibriPresence",
            jobj(vec![("kind", jstr(kind)), ("text", jstr(text))]),
        )]),
        GameCommand::PostSnapshot { summary } => jobj(vec![(
            "PostSnapshot",
            jobj(vec![(
                "summary",
                jobj(vec![
                    ("username", jstr(&summary.username)),
                    ("dimension", jstr(&summary.dimension)),
                    (
                        "position",
                        jobj(vec![
                            ("x", jint(summary.x)),
                            ("y", jint(summary.y)),
                            ("z", jint(summary.z)),
                        ]),
                    ),
                    ("health", jstr(&summary.health)),
                    ("food", jint(summary.food)),
                    ("xp_level", jint(summary.xp_level)),
                    ("game_mode", jstr(&summary.game_mode)),
                ]),
            )]),
        )]),
        GameCommand::PostSkin {
            skin_path,
            cape_path,
            model,
        } => jobj(vec![(
            "PostSkin",
            jobj(vec![
                ("skin_path", jopt_str(skin_path)),
                ("cape_path", jopt_str(cape_path)),
                ("model", jstr(model)),
            ]),
        )]),
        GameCommand::PostServerProfile => jstr("PostServerProfile"),
        GameCommand::PostServerProfileAsServer => jstr("PostServerProfileAsServer"),
        GameCommand::FetchList { reference } => jobj(vec![(
            "FetchList",
            jobj(vec![("reference", jstr(reference))]),
        )]),
        GameCommand::CheckGate { list, subject } => jobj(vec![(
            "CheckGate",
            jobj(vec![("list", jstr(list)), ("subject", jstr(subject))]),
        )]),
        GameCommand::FetchChat { subject } => {
            jobj(vec![("FetchChat", jobj(vec![("subject", jstr(subject))]))])
        }
        GameCommand::FetchLabels { subject } => jobj(vec![(
            "FetchLabels",
            jobj(vec![("subject", jstr(subject))]),
        )]),
        GameCommand::FetchLabelerLabels { labeler, subject } => jobj(vec![(
            "FetchLabelerLabels",
            jobj(vec![
                ("labeler", jstr(labeler)),
                ("subject", jopt_str(subject)),
            ]),
        )]),
        GameCommand::FetchPlayer { subject } => jobj(vec![(
            "FetchPlayer",
            jobj(vec![("subject", jstr(subject))]),
        )]),
        GameCommand::FetchProfile { subject } => jobj(vec![(
            "FetchProfile",
            jobj(vec![("subject", jstr(subject))]),
        )]),
        GameCommand::FetchPlayerSkin { subject } => jobj(vec![(
            "FetchPlayerSkin",
            jobj(vec![("subject", jstr(subject))]),
        )]),
        GameCommand::FetchServerProfile { subject } => jobj(vec![(
            "FetchServerProfile",
            jobj(vec![("subject", jstr(subject))]),
        )]),
        GameCommand::FetchGraph { kind, subject } => jobj(vec![(
            "FetchGraph",
            jobj(vec![("kind", jstr(kind)), ("subject", jstr(subject))]),
        )]),
        GameCommand::GraphWrite { action, subject } => jobj(vec![(
            "GraphWrite",
            jobj(vec![("action", jstr(action)), ("subject", jstr(subject))]),
        )]),
        GameCommand::Reply { uri, text } => jobj(vec![(
            "Reply",
            jobj(vec![("uri", jstr(uri)), ("text", jstr(text))]),
        )]),
        GameCommand::Like { uri } => jobj(vec![("Like", jobj(vec![("uri", jstr(uri))]))]),
        GameCommand::Repost { uri } => jobj(vec![("Repost", jobj(vec![("uri", jstr(uri))]))]),
        GameCommand::PostDeathMessage { text } => {
            jobj(vec![("PostDeathMessage", jobj(vec![("text", jstr(text))]))])
        }
        GameCommand::Logout => jstr("Logout"),
        GameCommand::Shutdown => jstr("Shutdown"),
    };
    let mut out = String::new();
    write_json(&value, &mut out);
    out
}

fn decode_verdict(value: &Json) -> Option<EndorsementVerdict> {
    match value {
        Json::Str(s) => match s.as_str() {
            "UnknownEndorser" => Some(EndorsementVerdict::UnknownEndorser),
            "BadSignature" => Some(EndorsementVerdict::BadSignature),
            "Malformed" => Some(EndorsementVerdict::Malformed),
            _ => None,
        },
        Json::Obj(pairs) if pairs.len() == 1 && pairs[0].0 == "Verified" => {
            let obj = as_obj(&pairs[0].1)?;
            Some(EndorsementVerdict::Verified {
                endorser: req_str(obj, "endorser")?,
            })
        }
        _ => None,
    }
}

fn decode_session(value: &Json) -> Option<SessionState> {
    match value {
        Json::Str(s) => match s.as_str() {
            "LoggedOut" => Some(SessionState::LoggedOut),
            "LoggingIn" => Some(SessionState::LoggingIn),
            _ => None,
        },
        Json::Obj(pairs) if pairs.len() == 1 => match pairs[0].0.as_str() {
            "LoggedIn" => {
                let obj = as_obj(&pairs[0].1)?;
                Some(SessionState::LoggedIn {
                    did: req_str(obj, "did")?,
                })
            }
            "Failed" => {
                let obj = as_obj(&pairs[0].1)?;
                Some(SessionState::Failed {
                    message: req_str(obj, "message")?,
                })
            }
            _ => None,
        },
        _ => None,
    }
}

fn decode_notification(value: &Json) -> Option<NotificationDigest> {
    let obj = as_obj(value)?;
    Some(NotificationDigest {
        uri: req_str(obj, "uri")?,
        author_handle: req_str(obj, "author_handle")?,
        reason: req_str(obj, "reason")?,
        text_snippet: req_str(obj, "text_snippet")?,
    })
}

fn decode_label(value: &Json) -> Option<LabelDigest> {
    let obj = as_obj(value)?;
    Some(LabelDigest {
        val: req_str(obj, "val")?,
        src: req_str(obj, "src")?,
        neg: jget(obj, "neg").and_then(as_bool).unwrap_or(false),
        cts: req_str(obj, "cts")?,
        // `exp` is skipped entirely when absent (skip_serializing_if).
        exp: jget(obj, "exp").and_then(as_opt_str).flatten(),
    })
}

fn decode_graph_member(value: &Json) -> Option<GraphMember> {
    let obj = as_obj(value)?;
    Some(GraphMember {
        did: req_str(obj, "did")?,
        handle: req_str(obj, "handle")?,
        display_name: req_str(obj, "display_name")?,
    })
}

fn decode_chat_digest(value: &Json) -> Option<ChatDigest> {
    let obj = as_obj(value)?;
    Some(ChatDigest {
        rkey: req_str(obj, "rkey")?,
        at_uri: req_str(obj, "at_uri")?,
        sent_at: req_str(obj, "sent_at")?,
        server: req_str(obj, "server")?,
        message: req_str(obj, "message")?,
    })
}

fn decode_player_digest(value: &Json) -> Option<PlayerDigest> {
    let obj = as_obj(value)?;
    Some(PlayerDigest {
        did: req_str(obj, "did")?,
        rkey: req_str(obj, "rkey")?,
        at_uri: req_str(obj, "at_uri")?,
        version: req_int(obj, "version")? as i64,
        issued_at: req_str(obj, "issued_at")?,
        username: req_str(obj, "username")?,
        dimension: req_str(obj, "dimension")?,
        x: req_int(obj, "x")? as i64,
        y: req_int(obj, "y")? as i64,
        z: req_int(obj, "z")? as i64,
        health: req_str(obj, "health")?,
        food: req_int(obj, "food")? as i64,
        xp_level: req_int(obj, "xp_level")? as i64,
        game_mode: req_str(obj, "game_mode")?,
        verdict: decode_verdict(jget(obj, "verdict")?)?,
    })
}

fn decode_profile_digest(value: &Json) -> Option<ProfileDigest> {
    let obj = as_obj(value)?;
    Some(ProfileDigest {
        did: req_str(obj, "did")?,
        handle: req_str(obj, "handle")?,
        display_name: req_str(obj, "display_name")?,
        description: req_str(obj, "description")?,
        pronouns: jget(obj, "pronouns").and_then(as_opt_str).flatten(),
        avatar: req_str(obj, "avatar")?,
    })
}

fn decode_blob_digest(value: &Json) -> Option<BlobDigest> {
    let obj = as_obj(value)?;
    Some(BlobDigest {
        cid: req_str(obj, "cid")?,
        mime_type: req_str(obj, "mime_type")?,
        size: req_int(obj, "size")? as u64,
        path: jget(obj, "path").and_then(as_opt_str)?,
    })
}

fn decode_skin_digest(value: &Json) -> Option<SkinDigest> {
    let obj = as_obj(value)?;
    Some(SkinDigest {
        did: req_str(obj, "did")?,
        rkey: req_str(obj, "rkey")?,
        at_uri: req_str(obj, "at_uri")?,
        model: req_str(obj, "model")?,
        timestamp: req_str(obj, "timestamp")?,
        verdict: decode_verdict(jget(obj, "verdict")?)?,
        skin: match jget(obj, "skin")? {
            Json::Null => None,
            other => decode_blob_digest(other),
        },
        cape: match jget(obj, "cape")? {
            Json::Null => None,
            other => decode_blob_digest(other),
        },
    })
}

fn decode_server_verdict(value: &Json) -> Option<ServerVerdict> {
    match value {
        Json::Str(s) => match s.as_str() {
            "Unknown" => Some(ServerVerdict::Unknown),
            "Malformed" => Some(ServerVerdict::Malformed),
            _ => None,
        },
        Json::Obj(pairs) if pairs.len() == 1 => match pairs[0].0.as_str() {
            "Trusted" => {
                let obj = as_obj(&pairs[0].1)?;
                Some(ServerVerdict::Trusted {
                    key_id: req_str(obj, "key_id")?,
                })
            }
            "KeyMismatch" => {
                let obj = as_obj(&pairs[0].1)?;
                Some(ServerVerdict::KeyMismatch {
                    key_id: req_str(obj, "key_id")?,
                })
            }
            _ => None,
        },
        _ => None,
    }
}

fn decode_server_digest(value: &Json) -> Option<ServerDigest> {
    let obj = as_obj(value)?;
    let capabilities = as_arr(jget(obj, "capabilities")?)?
        .iter()
        .map(|c| as_str(c).map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(ServerDigest {
        did: req_str(obj, "did")?,
        rkey: req_str(obj, "rkey")?,
        at_uri: req_str(obj, "at_uri")?,
        name: req_str(obj, "name")?,
        description: req_str(obj, "description")?,
        address: req_str(obj, "address")?,
        minecraft_version: req_str(obj, "minecraft_version")?,
        atpcraft_version: req_str(obj, "atpcraft_version")?,
        capabilities,
        operator_handle: req_str(obj, "operator_handle")?,
        created_at: jget(obj, "created_at").and_then(as_opt_str)?,
        updated_at: req_str(obj, "updated_at")?,
        signing_key_id: req_str(obj, "signing_key_id")?,
        verdict: decode_server_verdict(jget(obj, "verdict")?)?,
    })
}

/// Decode one JSON line from the sidecar worker into a game-side event.
/// Returns `None` on any shape mismatch (the caller reports a Failed event).
fn decode_event(line: &str) -> Option<ModEvent> {
    let Json::Obj(top) = parse_json(line)? else {
        return None;
    };
    let (tag, payload) = top.first()?;
    let payload_obj = || as_obj(payload);
    match tag.as_str() {
        "Status" => as_str(payload).map(|text| ModEvent::Status(text.to_string())),
        "Session" => decode_session(payload).map(ModEvent::Session),
        "ServerAccount" => {
            let obj = as_obj(payload)?;
            Some(ModEvent::ServerAccount {
                state: decode_session(jget(obj, "state")?)?,
                handle: jget(obj, "handle").and_then(as_opt_str).flatten(),
                did: jget(obj, "did").and_then(as_opt_str).flatten(),
            })
        }
        "Posted" => Some(ModEvent::Posted {
            uri: req_str(payload_obj()?, "uri")?,
        }),
        "Failed" => Some(ModEvent::Failed {
            message: req_str(payload_obj()?, "message")?,
        }),
        "OAuthAuthorize" => Some(ModEvent::OAuthAuthorize {
            url: req_str(payload_obj()?, "url")?,
        }),
        "Notifications" => {
            let obj = payload_obj()?;
            let items = as_arr(jget(obj, "items")?)?
                .iter()
                .map(decode_notification)
                .collect::<Option<Vec<_>>>()?;
            Some(ModEvent::Notifications { items })
        }
        "ListMembers" => {
            let obj = payload_obj()?;
            let members = as_arr(jget(obj, "members")?)?
                .iter()
                .map(|m| as_str(m).map(str::to_string))
                .collect::<Option<Vec<_>>>()?;
            Some(ModEvent::ListMembers {
                list: req_str(obj, "list")?,
                members,
            })
        }
        "GateDecision" => {
            let obj = payload_obj()?;
            Some(ModEvent::GateDecision {
                list: req_str(obj, "list")?,
                subject: req_str(obj, "subject")?,
                allowed: jget(obj, "allowed").and_then(as_bool)?,
            })
        }
        "ChatHistory" => {
            let obj = payload_obj()?;
            let items = as_arr(jget(obj, "items")?)?
                .iter()
                .map(decode_chat_digest)
                .collect::<Option<Vec<_>>>()?;
            Some(ModEvent::ChatHistory {
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                total: req_int(obj, "total")? as usize,
                items,
            })
        }
        "Labels" => {
            let obj = payload_obj()?;
            let labels = as_arr(jget(obj, "labels")?)?
                .iter()
                .map(decode_label)
                .collect::<Option<Vec<_>>>()?;
            Some(ModEvent::Labels {
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                labels,
            })
        }
        "PlayerSnapshot" => {
            let obj = payload_obj()?;
            let snapshot = match jget(obj, "snapshot")? {
                Json::Null => None,
                other => Some(decode_player_digest(other)?),
            };
            Some(ModEvent::PlayerSnapshot {
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                snapshot: Box::new(snapshot),
            })
        }
        "Profile" => {
            let obj = payload_obj()?;
            Some(ModEvent::Profile {
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                profile: Box::new(decode_profile_digest(jget(obj, "profile")?)?),
            })
        }
        "PlayerSkin" => {
            let obj = payload_obj()?;
            let skin = match jget(obj, "skin")? {
                Json::Null => None,
                other => Some(decode_skin_digest(other)?),
            };
            Some(ModEvent::PlayerSkin {
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                skin: Box::new(skin),
            })
        }
        "ServerProfile" => {
            let obj = payload_obj()?;
            let profile = match jget(obj, "profile")? {
                Json::Null => None,
                other => Some(decode_server_digest(other)?),
            };
            Some(ModEvent::ServerProfile {
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                profile: Box::new(profile),
            })
        }
        "SocialGraph" => {
            let obj = payload_obj()?;
            let members = as_arr(jget(obj, "members")?)?
                .iter()
                .map(decode_graph_member)
                .collect::<Option<Vec<_>>>()?;
            Some(ModEvent::SocialGraph {
                kind: req_str(obj, "kind")?,
                subject: req_str(obj, "subject")?,
                via: req_str(obj, "via")?,
                members,
            })
        }
        "GraphChanged" => {
            let obj = payload_obj()?;
            Some(ModEvent::GraphChanged {
                action: req_str(obj, "action")?,
                subject: req_str(obj, "subject")?,
            })
        }
        "Colibri" => {
            let obj = payload_obj()?;
            Some(ModEvent::Colibri {
                active: jget(obj, "active").and_then(as_bool)?,
                connected: jget(obj, "connected").and_then(as_bool)?,
                channel: req_str(obj, "channel")?,
                error: jget(obj, "error").and_then(as_opt_str).flatten(),
            })
        }
        "ColibriMessage" => {
            let obj = payload_obj()?;
            Some(ModEvent::ColibriMessage {
                handle: req_str(obj, "handle")?,
                text: req_str(obj, "text")?,
            })
        }
        "ColibriProfileMissing" => {
            let obj = payload_obj()?;
            Some(ModEvent::ColibriProfileMissing {
                url: req_str(obj, "url")?,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `VERSION` is shown in Options > Mods and names the release, while the
    /// package version drives the worker binary and the tag. Pin them together
    /// so a release bump cannot leave the mod row behind. When horizon-mixins
    /// injects this module into the game crate, `CARGO_PKG_NAME` is Horizon's,
    /// so the check only applies to the standalone library package.
    #[test]
    fn version_matches_the_package() {
        if env!("CARGO_PKG_NAME") == "atmospheric-horizon" {
            assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        }
    }

    // The channel endpoints live in statics, so the tests share one entry
    // point instead of racing over them in parallel.
    #[test]
    fn install_forwards_and_drains() {
        let (command_rx, event_tx) = install().expect("first install succeeds");
        assert!(install().is_none(), "second install fails");
        // No worker event yet: the HUD lines are empty, so nothing is drawn.
        assert!(status_lines().is_empty());
        assert!(status_line().is_empty());

        // Game side -> worker: the commands the hooks send.
        login();
        login_oauth("someone.example");
        post_text("hello from the game");
        post_chat("a chat line", true);
        colibri_start();
        colibri_reconnect();
        colibri_status();
        colibri_stop();
        colibri_send("hello colibri");
        post_colibri_notice("Alex joined the game");
        post_colibri_handled(Some("join"));
        post_colibri_presence("join", "Alex joined the game");
        post_snapshot(snapshot_from_player(
            "",
            "minecraft:overworld",
            -24.5,
            67.0,
            -2.5,
        ));
        post_death_message("Player was slain by Zombie");
        fetch_labels("at://did:plc:example/app.bsky.feed.post/3abc");
        fetch_labeler_labels("did:plc:labeler", Some("did:plc:example"));
        fetch_labeler_labels("did:plc:labeler", None);
        fetch_player("someone.example");
        fetch_profile("someone.example");
        fetch_player_skin("someone.example");
        fetch_server_profile("someone.example");
        fetch_graph("follows", "someone.example");
        follow("someone.example");
        unfollow("someone.example");
        block("someone.example");
        unblock("someone.example");
        mute("someone.example");
        unmute("someone.example");
        reply("at://did:plc:example/app.bsky.feed.post/3abc", "nice");
        like("at://did:plc:example/app.bsky.feed.post/3abc");
        repost("at://did:plc:example/app.bsky.feed.post/3abc");
        check_gate(
            "at://did:plc:example/app.bsky.graph.list/3abc",
            "did:plc:example",
        );
        fetch_chat("someone.example");
        logout();
        assert!(!post_screenshot("no screenshot yet"));
        remember_screenshot("/tmp/shots/2026-11-12_07.00.00.png");
        assert_eq!(
            latest_screenshot().as_deref(),
            Some("/tmp/shots/2026-11-12_07.00.00.png")
        );
        assert!(post_screenshot("Screenshot from Horizon"));
        save_config(Settings {
            pds_url: "https://pds.example".into(),
            identifier: "someone.example".into(),
            app_password: "abcd-1234-efgh-5678".into(),
            poll_interval_secs: 30,
            post_death_messages: false,
            post_chat_messages: false,
            chat_server_id: "horizon-client".into(),
        });
        shutdown();
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::Login)
        ));
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::LoginOAuth { handle }) => assert_eq!(handle, "someone.example"),
            other => panic!("expected LoginOAuth, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::PostText { text }) => assert_eq!(text, "hello from the game"),
            other => panic!("expected PostText, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::PostChat { message, automatic }) => {
                assert_eq!(message, "a chat line");
                assert!(automatic);
            }
            other => panic!("expected PostChat, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::ColibriSend { text, automatic }) => {
                assert_eq!(text, "a chat line");
                assert!(automatic);
            }
            other => panic!("expected automatic ColibriSend, got {:?}", other),
        }
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::ColibriStart)
        ));
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::ColibriReconnect)
        ));
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::ColibriStatus)
        ));
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::ColibriStop)
        ));
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::ColibriSend { text, automatic }) => {
                assert_eq!(text, "hello colibri");
                assert!(!automatic);
            }
            other => panic!("expected explicit ColibriSend, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::ColibriNotice { text }) => {
                assert_eq!(text, "Alex joined the game")
            }
            other => panic!("expected ColibriNotice, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::ColibriHandled { kind }) => {
                assert_eq!(kind.as_deref(), Some("join"));
            }
            other => panic!("expected ColibriHandled, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::ColibriPresence { kind, text }) => {
                assert_eq!(kind, "join");
                assert_eq!(text, "Alex joined the game");
            }
            other => panic!("expected ColibriPresence, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::PostSnapshot { summary }) => {
                assert_eq!(summary.dimension, "minecraft:overworld");
                assert_eq!((summary.x, summary.y, summary.z), (-25, 67, -3));
            }
            other => panic!("expected PostSnapshot, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::PostDeathMessage { text }) => {
                assert_eq!(text, "Player was slain by Zombie")
            }
            other => panic!("expected PostDeathMessage, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchLabels { subject }) => {
                assert_eq!(subject, "at://did:plc:example/app.bsky.feed.post/3abc")
            }
            other => panic!("expected FetchLabels, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchLabelerLabels { labeler, subject }) => {
                assert_eq!(labeler, "did:plc:labeler");
                assert_eq!(subject.as_deref(), Some("did:plc:example"));
            }
            other => panic!("expected FetchLabelerLabels, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchLabelerLabels { subject, .. }) => assert_eq!(subject, None),
            other => panic!("expected FetchLabelerLabels, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchPlayer { subject }) => assert_eq!(subject, "someone.example"),
            other => panic!("expected FetchPlayer, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchProfile { subject }) => assert_eq!(subject, "someone.example"),
            other => panic!("expected FetchProfile, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchPlayerSkin { subject }) => assert_eq!(subject, "someone.example"),
            other => panic!("expected FetchPlayerSkin, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchServerProfile { subject }) => {
                assert_eq!(subject, "someone.example")
            }
            other => panic!("expected FetchServerProfile, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchGraph { kind, subject }) => {
                assert_eq!(kind, "follows");
                assert_eq!(subject, "someone.example");
            }
            other => panic!("expected FetchGraph, got {:?}", other),
        }
        for expected in ["follow", "unfollow", "block", "unblock", "mute", "unmute"] {
            match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(GameCommand::GraphWrite { action, subject }) => {
                    assert_eq!(action, expected);
                    assert_eq!(subject, "someone.example");
                }
                other => panic!("expected GraphWrite({expected}), got {:?}", other),
            }
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::Reply { uri, text }) => {
                assert_eq!(uri, "at://did:plc:example/app.bsky.feed.post/3abc");
                assert_eq!(text, "nice");
            }
            other => panic!("expected Reply, got {:?}", other),
        }
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::Like { .. })
        ));
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::Repost { .. })
        ));
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::CheckGate { list, subject }) => {
                assert_eq!(list, "at://did:plc:example/app.bsky.graph.list/3abc");
                assert_eq!(subject, "did:plc:example");
            }
            other => panic!("expected CheckGate, got {:?}", other),
        }
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::FetchChat { subject }) => assert_eq!(subject, "someone.example"),
            other => panic!("expected FetchChat, got {:?}", other),
        }
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::Logout)
        ));
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::PostImage {
                path,
                alt_text,
                text,
            }) => {
                assert_eq!(path, "/tmp/shots/2026-11-12_07.00.00.png");
                assert_eq!(alt_text, "Screenshot from Horizon");
                assert_eq!(text, None);
            }
            other => panic!("expected PostImage, got {:?}", other),
        }
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::SaveConfig { .. })
        ));
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::Shutdown)
        ));

        // Worker -> game side: drained events also feed the status line.
        event_tx
            .send(ModEvent::Session(SessionState::LoggedIn {
                did: "did:plc:example".into(),
            }))
            .unwrap();
        event_tx
            .send(ModEvent::Posted {
                uri: "at://did:plc:example/app.bsky.feed.post/3abc".into(),
            })
            .unwrap();
        let drained = drain_events(16);
        assert_eq!(drained.len(), 2);
        // Both events stay visible as two lines (not just the latest, #7).
        assert_eq!(
            status_lines(),
            vec![
                "atmospheric: logged in as did:plc:example".to_string(),
                "atmospheric: posted at://did:plc:example/app.bsky.feed.post/3abc".to_string(),
            ]
        );
        assert_eq!(
            status_line(),
            "atmospheric: posted at://did:plc:example/app.bsky.feed.post/3abc"
        );

        // The drain is bounded, and later events still land.
        event_tx
            .send(ModEvent::Failed {
                message: "boom".into(),
            })
            .unwrap();
        event_tx
            .send(ModEvent::Notifications {
                items: vec![NotificationDigest {
                    uri: "at://did:plc:bob/app.bsky.feed.post/2".into(),
                    author_handle: "bob.test".into(),
                    reason: "reply".into(),
                    text_snippet: "nice post!".into(),
                }],
            })
            .unwrap();
        assert_eq!(drain_events(1).len(), 1);
        assert_eq!(drain_events(16).len(), 1);
        assert_eq!(status_line(), "atmospheric: 1 new: reply from bob.test");

        // The worker side stays usable after the game side drops it.
        drop(event_tx);
        post_text("still works");
    }

    #[test]
    fn status_lines_expire_after_the_ttl() {
        let now = std::time::Instant::now();
        let old = now
            .checked_sub(STATUS_LINE_TTL + std::time::Duration::from_secs(1))
            .expect("the test clock supports this");
        let mut status = vec![("stale".to_string(), old), ("fresh".to_string(), now)];
        prune_expired(&mut status, now);
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].0, "fresh");
    }

    #[test]
    fn status_text_covers_every_event() {
        assert_eq!(
            status_text(&ModEvent::Profile {
                subject: "did:plc:alice".into(),
                via: "https://pds.example".into(),
                profile: Box::new(ProfileDigest {
                    did: "did:plc:alice".into(),
                    handle: "alice.test".into(),
                    display_name: "Alice".into(),
                    description: "AppView bio".into(),
                    pronouns: Some("they/them".into()),
                    avatar: "https://cdn.example/alice.png".into(),
                }),
            }),
            "atmospheric: Alice (@alice.test) (they/them) — AppView bio"
        );
        assert_eq!(
            status_text(&ModEvent::Profile {
                subject: "did:plc:bob".into(),
                via: "https://pds.example".into(),
                profile: Box::new(ProfileDigest {
                    did: "did:plc:bob".into(),
                    handle: String::new(),
                    display_name: String::new(),
                    description: String::new(),
                    pronouns: None,
                    avatar: String::new(),
                }),
            }),
            "atmospheric: did:plc:bob"
        );
        assert_eq!(
            status_text(&ModEvent::GraphChanged {
                action: "follow".into(),
                subject: "did:plc:x".into(),
            }),
            "atmospheric: followed did:plc:x"
        );
        assert_eq!(
            status_text(&ModEvent::GraphChanged {
                action: "unmute".into(),
                subject: "did:plc:x".into(),
            }),
            "atmospheric: unmuted did:plc:x"
        );
        assert_eq!(
            status_text(&ModEvent::SocialGraph {
                kind: "follows".into(),
                subject: "did:plc:x".into(),
                via: "https://public.api.bsky.app".into(),
                members: vec![
                    GraphMember {
                        did: "did:plc:a".into(),
                        handle: "a.test".into(),
                        display_name: "A".into(),
                    },
                    GraphMember {
                        did: "did:plc:b".into(),
                        handle: String::new(),
                        display_name: String::new(),
                    },
                ],
            }),
            "atmospheric: follows of did:plc:x via https://public.api.bsky.app: 2 account(s) — a.test, did:plc:b"
        );
        assert_eq!(
            status_text(&ModEvent::SocialGraph {
                kind: "mutes".into(),
                subject: "did:plc:x".into(),
                via: "https://public.api.bsky.app".into(),
                members: vec![],
            }),
            "atmospheric: no mutes for did:plc:x (via https://public.api.bsky.app)"
        );
        assert_eq!(
            status_text(&ModEvent::Session(SessionState::LoggedIn {
                did: "did:plc:x".into()
            })),
            "atmospheric: logged in as did:plc:x"
        );
        assert_eq!(
            status_text(&ModEvent::Session(SessionState::Failed {
                message: "nope".into()
            })),
            "atmospheric: login failed: nope"
        );
        assert_eq!(status_text(&ModEvent::Status("custom".into())), "custom");
        assert_eq!(
            status_text(&ModEvent::OAuthAuthorize {
                url: "https://auth.example/authorize?request_uri=x".into()
            }),
            "atmospheric: finish login at https://auth.example/authorize?request_uri=x"
        );
        assert_eq!(
            status_text(&ModEvent::ListMembers {
                list: "at://did:plc:x/app.bsky.graph.list/3abc".into(),
                members: vec!["did:plc:a".into(), "did:plc:b".into()],
            }),
            "atmospheric: list at://did:plc:x/app.bsky.graph.list/3abc has 2 members"
        );
        assert_eq!(
            status_text(&ModEvent::GateDecision {
                list: "at://did:plc:x/app.bsky.graph.list/3abc".into(),
                subject: "did:plc:a".into(),
                allowed: true,
            }),
            "atmospheric: gate at://did:plc:x/app.bsky.graph.list/3abc — did:plc:a ALLOWED"
        );
        assert_eq!(
            status_text(&ModEvent::GateDecision {
                list: "at://did:plc:x/app.bsky.graph.list/3abc".into(),
                subject: "did:plc:z".into(),
                allowed: false,
            }),
            "atmospheric: gate at://did:plc:x/app.bsky.graph.list/3abc — did:plc:z DENIED"
        );
        assert_eq!(
            status_text(&ModEvent::ChatHistory {
                subject: "did:plc:x".into(),
                via: "https://pds.example".into(),
                total: 12,
                items: vec![ChatDigest {
                    rkey: "3jub4stub0002".into(),
                    at_uri: "at://did:plc:x/at.atpcraft.player.chat/3jub4stub0002".into(),
                    sent_at: "2026-11-12T12:00:00.000Z".into(),
                    server: "srv".into(),
                    message: "hello there".into(),
                }],
            }),
            "atmospheric: 12 chat record(s) for did:plc:x (unsigned) — latest from srv: \"hello there\""
        );
        assert_eq!(
            status_text(&ModEvent::ChatHistory {
                subject: "did:plc:empty".into(),
                via: "https://pds.example".into(),
                total: 0,
                items: vec![],
            }),
            "atmospheric: no chat records for did:plc:empty"
        );
        assert_eq!(
            status_text(&ModEvent::Labels {
                subject: "did:plc:x".into(),
                via: "did:plc:labeler (https://labels.example)".into(),
                labels: vec![
                    LabelDigest {
                        val: "player".into(),
                        src: "did:plc:labeler".into(),
                        neg: false,
                        cts: "2026-11-12T10:00:00.000Z".into(),
                        exp: None,
                    },
                    LabelDigest {
                        val: "spam".into(),
                        src: "did:plc:labeler".into(),
                        neg: true,
                        cts: "2026-11-12T11:00:00.000Z".into(),
                        exp: None,
                    },
                ],
            }),
            "atmospheric: 2 label(s) on did:plc:x via did:plc:labeler (https://labels.example): player, -spam"
        );
        assert_eq!(
            status_text(&ModEvent::Labels {
                subject: "at://did:plc:x/app.bsky.feed.post/1".into(),
                via: "https://public.api.bsky.app".into(),
                labels: vec![],
            }),
            "atmospheric: no labels on at://did:plc:x/app.bsky.feed.post/1"
        );
        assert_eq!(
            status_text(&ModEvent::PlayerSnapshot {
                subject: "did:plc:x".into(),
                via: "https://pds.example".into(),
                snapshot: Box::new(Some(PlayerDigest {
                    did: "did:plc:x".into(),
                    rkey: "keyid0000000001".into(),
                    at_uri: "at://did:plc:x/at.atpcraft.player.snapshot/keyid0000000001".into(),
                    version: 7,
                    issued_at: "2026-11-12T12:00:00.000Z".into(),
                    username: "Steve".into(),
                    dimension: "minecraft:overworld".into(),
                    x: -25,
                    y: 67,
                    z: -3,
                    health: "19.5".into(),
                    food: 20,
                    xp_level: 3,
                    game_mode: "survival".into(),
                    verdict: EndorsementVerdict::Verified {
                        endorser: "keyid0000000001".into()
                    },
                })),
            }),
            "atmospheric: Steve v7 in minecraft:overworld at -25, 67, -3 — verified by keyid0000000001"
        );
        assert_eq!(
            status_text(&ModEvent::PlayerSnapshot {
                subject: "did:plc:x".into(),
                via: "https://pds.example".into(),
                snapshot: Box::new(Some(PlayerDigest {
                    did: "did:plc:x".into(),
                    rkey: "k".into(),
                    at_uri: "at://did:plc:x/at.atpcraft.player.snapshot/k".into(),
                    version: 2,
                    issued_at: String::new(),
                    username: "Mallory".into(),
                    dimension: "minecraft:the_end".into(),
                    x: 0,
                    y: 0,
                    z: 0,
                    health: "20".into(),
                    food: 20,
                    xp_level: 0,
                    game_mode: "survival".into(),
                    verdict: EndorsementVerdict::BadSignature,
                })),
            }),
            "atmospheric: Mallory v2 in minecraft:the_end at 0, 0, 0 — BAD SIGNATURE"
        );
        assert_eq!(
            status_text(&ModEvent::PlayerSnapshot {
                subject: "did:plc:empty".into(),
                via: "https://pds.example".into(),
                snapshot: Box::new(None),
            }),
            "atmospheric: no snapshot records for did:plc:empty"
        );
        assert_eq!(
            status_text(&ModEvent::PlayerSkin {
                subject: "did:plc:x".into(),
                via: "https://pds.example".into(),
                skin: Box::new(Some(SkinDigest {
                    did: "did:plc:x".into(),
                    rkey: "keyid0000000001".into(),
                    at_uri: "at://did:plc:x/at.atpcraft.player.skin/keyid0000000001".into(),
                    model: "slim".into(),
                    timestamp: "2026-11-12T12:00:00.000Z".into(),
                    verdict: EndorsementVerdict::Verified {
                        endorser: "keyid0000000001".into()
                    },
                    skin: Some(BlobDigest {
                        cid: "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy".into(),
                        mime_type: "image/png".into(),
                        size: 42,
                        path: Some("/data/atmospheric/skins/bafkreigh2.png".into()),
                    }),
                    cape: None,
                })),
            }),
            "atmospheric: did:plc:x skin model slim — skin bafkreigh2ak… saved to /data/atmospheric/skins/bafkreigh2.png, cape none — verified by keyid0000000001"
        );
        assert_eq!(
            status_text(&ModEvent::PlayerSkin {
                subject: "did:plc:empty".into(),
                via: "https://pds.example".into(),
                skin: Box::new(None),
            }),
            "atmospheric: no skin records for did:plc:empty"
        );
        assert_eq!(
            status_text(&ModEvent::PlayerSkin {
                subject: "did:plc:y".into(),
                via: "https://pds.example".into(),
                skin: Box::new(Some(SkinDigest {
                    did: "did:plc:y".into(),
                    rkey: "k".into(),
                    at_uri: "at://did:plc:y/at.atpcraft.player.skin/k".into(),
                    model: "default".into(),
                    timestamp: String::new(),
                    verdict: EndorsementVerdict::BadSignature,
                    skin: Some(BlobDigest {
                        cid: "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy".into(),
                        mime_type: "image/png".into(),
                        size: 7,
                        path: None,
                    }),
                    cape: None,
                })),
            }),
            "atmospheric: did:plc:y skin model default — skin bafkreibme22… 7 B, not saved, cape none — BAD SIGNATURE"
        );
    }

    #[test]
    fn colibri_notice_keys_match_atpcraft() {
        for key in [
            "multiplayer.player.joined",
            "multiplayer.player.joined.renamed",
            "multiplayer.player.left",
            "death.attack.player",
            "death.fell.accident.generic",
            "chat.type.advancement.task",
            "chat.type.advancement.goal",
            "chat.type.advancement.challenge",
        ] {
            assert!(is_colibri_notice(key), "{key}");
        }
        for key in ["chat.type.text", "", "chat.type.advancement"] {
            assert!(!is_colibri_notice(key), "{key}");
        }
    }

    #[test]
    fn location_line_reads_like_where() {
        assert_eq!(
            location_line(12.5, 64.0, -3.25, "minecraft:the_nether"),
            "At 12, 64, -4 in minecraft:the_nether — from Horizon"
        );
    }

    // ---- M17: the sidecar wire codec (the crate-side lib tests pin the same
    // shapes with serde_json, so the two sides cannot drift apart) ----

    fn sample_settings() -> Settings {
        Settings {
            pds_url: "https://pds.example".into(),
            identifier: "someone.example".into(),
            app_password: "abcd-1234-efgh-5678".into(),
            poll_interval_secs: 30,
            post_death_messages: false,
            post_chat_messages: true,
            chat_server_id: "horizon-client".into(),
        }
    }

    #[test]
    fn server_account_status_text() {
        assert_eq!(
            status_text(&ModEvent::ServerAccount {
                state: SessionState::LoggedOut,
                handle: None,
                did: None,
            }),
            "atmospheric: server account logged out"
        );
        assert_eq!(
            status_text(&ModEvent::ServerAccount {
                state: SessionState::LoggedIn {
                    did: "did:plc:server".into(),
                },
                handle: Some("server.test".into()),
                did: Some("did:plc:server".into()),
            }),
            "atmospheric: server account @server.test logged in"
        );
        assert_eq!(
            status_text(&ModEvent::Colibri {
                active: false,
                connected: false,
                channel: String::new(),
                error: None,
            }),
            "atmospheric: Colibri bridge off"
        );
        assert_eq!(
            status_text(&ModEvent::Colibri {
                active: true,
                connected: true,
                channel: "at://c".into(),
                error: None,
            }),
            "atmospheric: Colibri connected to at://c"
        );
        assert_eq!(
            status_text(&ModEvent::ColibriProfileMissing {
                url: "https://colibri.social".into(),
            }),
            "atmospheric: Colibri profile missing — open https://colibri.social"
        );
        assert_eq!(
            colibri_chat_lines(&ModEvent::ColibriProfileMissing {
                url: "https://colibri.social".into(),
            }),
            vec![
                "[Colibri] You need a Colibri profile to use the chat bridge.".to_string(),
                "[Colibri] Open Colibri: https://colibri.social".to_string(),
            ]
        );
    }

    #[test]
    fn encode_command_wire_shapes() {
        assert_eq!(encode_command(&GameCommand::Login), r#""Login""#);
        assert_eq!(
            encode_command(&GameCommand::LoginOAuth {
                handle: "a.test".into()
            }),
            r#"{"LoginOAuth":{"handle":"a.test"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::LoginServerOAuth {
                handle: "server.test".into()
            }),
            r#"{"LoginServerOAuth":{"handle":"server.test"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::ServerAccountStatus),
            r#""ServerAccountStatus""#
        );
        assert_eq!(
            encode_command(&GameCommand::LogoutServerAccount),
            r#""LogoutServerAccount""#
        );
        assert_eq!(
            encode_command(&GameCommand::SaveConfig {
                settings: sample_settings()
            }),
            r#"{"SaveConfig":{"config":{"pds_url":"https://pds.example","identifier":"someone.example","app_password":"abcd-1234-efgh-5678","poll_interval_secs":30,"post_death_messages":false,"post_chat_messages":true,"chat_server_id":"horizon-client"}}}"#
        );
        // String escaping matches serde_json: quote, backslash, newline.
        assert_eq!(
            encode_command(&GameCommand::PostText {
                text: "hi \"you\"\n\\".into()
            }),
            r#"{"PostText":{"text":"hi \"you\"\n\\"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::PostImage {
                path: "/tmp/s.png".into(),
                alt_text: "a shot".into(),
                text: None,
            }),
            r#"{"PostImage":{"path":"/tmp/s.png","alt_text":"a shot","text":null}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::RefreshNotifications),
            r#""RefreshNotifications""#
        );
        assert_eq!(
            encode_command(&GameCommand::PostChat {
                message: "m".into(),
                automatic: true,
            }),
            r#"{"PostChat":{"message":"m","automatic":true}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriStart),
            r#""ColibriStart""#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriReconnect),
            r#""ColibriReconnect""#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriStop),
            r#""ColibriStop""#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriStatus),
            r#""ColibriStatus""#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriSend {
                text: "hi".into(),
                automatic: false,
            }),
            r#"{"ColibriSend":{"text":"hi","automatic":false}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriNotice {
                text: "Alex joined the game".into()
            }),
            r#"{"ColibriNotice":{"text":"Alex joined the game"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriHandled {
                kind: Some("join".into())
            }),
            r#"{"ColibriHandled":{"kind":"join"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriHandled { kind: None }),
            r#"{"ColibriHandled":{"kind":null}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::ColibriPresence {
                kind: "leave".into(),
                text: "Alex left the game".into(),
            }),
            r#"{"ColibriPresence":{"kind":"leave","text":"Alex left the game"}}"#
        );
        // The flat game-side x/y/z map onto the nested `position` object.
        assert_eq!(
            encode_command(&GameCommand::PostSnapshot {
                summary: snapshot_from_player("Steve", "minecraft:overworld", -24.5, 67.0, -2.5)
            }),
            r#"{"PostSnapshot":{"summary":{"username":"Steve","dimension":"minecraft:overworld","position":{"x":-25,"y":67,"z":-3},"health":"20","food":20,"xp_level":0,"game_mode":"survival"}}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::PostSkin {
                skin_path: None,
                cape_path: Some("/c.png".into()),
                model: "slim".into(),
            }),
            r#"{"PostSkin":{"skin_path":null,"cape_path":"/c.png","model":"slim"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::PostServerProfile),
            r#""PostServerProfile""#
        );
        assert_eq!(
            encode_command(&GameCommand::PostServerProfileAsServer),
            r#""PostServerProfileAsServer""#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchList {
                reference: "at://did:plc:x/app.bsky.graph.list/3k".into()
            }),
            r#"{"FetchList":{"reference":"at://did:plc:x/app.bsky.graph.list/3k"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::CheckGate {
                list: "l".into(),
                subject: "s".into(),
            }),
            r#"{"CheckGate":{"list":"l","subject":"s"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchChat {
                subject: "did:plc:z".into()
            }),
            r#"{"FetchChat":{"subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchLabels {
                subject: "did:plc:z".into()
            }),
            r#"{"FetchLabels":{"subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchLabelerLabels {
                labeler: "did:plc:l".into(),
                subject: None,
            }),
            r#"{"FetchLabelerLabels":{"labeler":"did:plc:l","subject":null}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchPlayer {
                subject: "did:plc:z".into()
            }),
            r#"{"FetchPlayer":{"subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchProfile {
                subject: "did:plc:z".into()
            }),
            r#"{"FetchProfile":{"subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchPlayerSkin {
                subject: "did:plc:z".into()
            }),
            r#"{"FetchPlayerSkin":{"subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchServerProfile {
                subject: "did:plc:z".into()
            }),
            r#"{"FetchServerProfile":{"subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::FetchGraph {
                kind: "follows".into(),
                subject: "did:plc:z".into(),
            }),
            r#"{"FetchGraph":{"kind":"follows","subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::GraphWrite {
                action: "follow".into(),
                subject: "did:plc:z".into(),
            }),
            r#"{"GraphWrite":{"action":"follow","subject":"did:plc:z"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::Reply {
                uri: "at://did:plc:z/app.bsky.feed.post/1".into(),
                text: "nice".into(),
            }),
            r#"{"Reply":{"uri":"at://did:plc:z/app.bsky.feed.post/1","text":"nice"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::Like {
                uri: "at://did:plc:z/app.bsky.feed.post/1".into()
            }),
            r#"{"Like":{"uri":"at://did:plc:z/app.bsky.feed.post/1"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::Repost {
                uri: "at://did:plc:z/app.bsky.feed.post/1".into()
            }),
            r#"{"Repost":{"uri":"at://did:plc:z/app.bsky.feed.post/1"}}"#
        );
        assert_eq!(
            encode_command(&GameCommand::PostDeathMessage { text: "t".into() }),
            r#"{"PostDeathMessage":{"text":"t"}}"#
        );
        assert_eq!(encode_command(&GameCommand::Logout), r#""Logout""#);
        assert_eq!(encode_command(&GameCommand::Shutdown), r#""Shutdown""#);
    }

    #[test]
    fn decode_event_wire_shapes() {
        assert_eq!(
            decode_event(
                r#"{"SocialGraph":{"kind":"follows","subject":"did:plc:x","via":"https://public.api.bsky.app","members":[{"did":"did:plc:a","handle":"a.test","display_name":"A"}]}}"#
            ),
            Some(ModEvent::SocialGraph {
                kind: "follows".into(),
                subject: "did:plc:x".into(),
                via: "https://public.api.bsky.app".into(),
                members: vec![GraphMember {
                    did: "did:plc:a".into(),
                    handle: "a.test".into(),
                    display_name: "A".into(),
                }],
            })
        );
        assert_eq!(
            decode_event(r#"{"GraphChanged":{"action":"follow","subject":"did:plc:x"}}"#),
            Some(ModEvent::GraphChanged {
                action: "follow".into(),
                subject: "did:plc:x".into(),
            })
        );
        assert_eq!(
            decode_event(r#"{"Session":"LoggedOut"}"#),
            Some(ModEvent::Session(SessionState::LoggedOut))
        );
        assert_eq!(
            decode_event(r#"{"Session":"LoggingIn"}"#),
            Some(ModEvent::Session(SessionState::LoggingIn))
        );
        assert_eq!(
            decode_event(r#"{"Session":{"LoggedIn":{"did":"did:plc:x"}}}"#),
            Some(ModEvent::Session(SessionState::LoggedIn {
                did: "did:plc:x".into()
            }))
        );
        assert_eq!(
            decode_event(r#"{"Session":{"Failed":{"message":"m"}}}"#),
            Some(ModEvent::Session(SessionState::Failed {
                message: "m".into()
            }))
        );
        assert_eq!(
            decode_event(
                r#"{"ServerAccount":{"state":{"LoggedIn":{"did":"did:plc:server"}},"handle":"server.test","did":"did:plc:server"}}"#
            ),
            Some(ModEvent::ServerAccount {
                state: SessionState::LoggedIn {
                    did: "did:plc:server".into()
                },
                handle: Some("server.test".into()),
                did: Some("did:plc:server".into()),
            })
        );
        assert_eq!(
            decode_event(r#"{"ServerAccount":{"state":"LoggedOut","handle":null,"did":null}}"#),
            Some(ModEvent::ServerAccount {
                state: SessionState::LoggedOut,
                handle: None,
                did: None,
            })
        );
        assert_eq!(
            decode_event(r#"{"Posted":{"uri":"at://u"}}"#),
            Some(ModEvent::Posted {
                uri: "at://u".into()
            })
        );
        assert_eq!(
            decode_event(
                r#"{"Colibri":{"active":true,"connected":false,"channel":"at://c","error":null}}"#
            ),
            Some(ModEvent::Colibri {
                active: true,
                connected: false,
                channel: "at://c".into(),
                error: None,
            })
        );
        assert_eq!(
            decode_event(r#"{"ColibriMessage":{"handle":"a.test","text":"hi"}}"#),
            Some(ModEvent::ColibriMessage {
                handle: "a.test".into(),
                text: "hi".into(),
            })
        );
        assert_eq!(
            decode_event(r#"{"ColibriProfileMissing":{"url":"https://colibri.social"}}"#),
            Some(ModEvent::ColibriProfileMissing {
                url: "https://colibri.social".into(),
            })
        );
        assert_eq!(
            decode_event(r#"{"Status":"pending approval"}"#),
            Some(ModEvent::Status("pending approval".into()))
        );
        assert_eq!(
            decode_event(r#"{"Failed":{"message":"m"}}"#),
            Some(ModEvent::Failed {
                message: "m".into()
            })
        );
        assert_eq!(
            decode_event(r#"{"OAuthAuthorize":{"url":"https://as.example"}}"#),
            Some(ModEvent::OAuthAuthorize {
                url: "https://as.example".into()
            })
        );
        assert_eq!(
            decode_event(
                r#"{"Notifications":{"items":[{"uri":"u","author_handle":"a.test","reason":"like","text_snippet":"hi"}]}}"#
            ),
            Some(ModEvent::Notifications {
                items: vec![NotificationDigest {
                    uri: "u".into(),
                    author_handle: "a.test".into(),
                    reason: "like".into(),
                    text_snippet: "hi".into(),
                }]
            })
        );
        assert_eq!(
            decode_event(r#"{"ListMembers":{"list":"l","members":["did:plc:a","did:plc:b"]}}"#),
            Some(ModEvent::ListMembers {
                list: "l".into(),
                members: vec!["did:plc:a".into(), "did:plc:b".into()]
            })
        );
        assert_eq!(
            decode_event(r#"{"GateDecision":{"list":"l","subject":"s","allowed":true}}"#),
            Some(ModEvent::GateDecision {
                list: "l".into(),
                subject: "s".into(),
                allowed: true
            })
        );
        assert_eq!(
            decode_event(
                r#"{"ChatHistory":{"subject":"s","via":"https://pds.example","total":2,"items":[{"rkey":"r","at_uri":"at://u","sent_at":"t","server":"srv","message":"hi"}]}}"#
            ),
            Some(ModEvent::ChatHistory {
                subject: "s".into(),
                via: "https://pds.example".into(),
                total: 2,
                items: vec![ChatDigest {
                    rkey: "r".into(),
                    at_uri: "at://u".into(),
                    sent_at: "t".into(),
                    server: "srv".into(),
                    message: "hi".into(),
                }]
            })
        );
        // `exp` is skipped when absent (skip_serializing_if) and present otherwise.
        assert_eq!(
            decode_event(
                r#"{"Labels":{"subject":"s","via":"v","labels":[{"val":"online","src":"did:plc:l","neg":false,"cts":"t"}]}}"#
            ),
            Some(ModEvent::Labels {
                subject: "s".into(),
                via: "v".into(),
                labels: vec![LabelDigest {
                    val: "online".into(),
                    src: "did:plc:l".into(),
                    neg: false,
                    cts: "t".into(),
                    exp: None,
                }]
            })
        );
        assert_eq!(
            decode_event(
                r#"{"Labels":{"subject":"s","via":"v","labels":[{"val":"spam","src":"did:plc:l","neg":true,"cts":"t","exp":"e"}]}}"#
            ),
            Some(ModEvent::Labels {
                subject: "s".into(),
                via: "v".into(),
                labels: vec![LabelDigest {
                    val: "spam".into(),
                    src: "did:plc:l".into(),
                    neg: true,
                    cts: "t".into(),
                    exp: Some("e".into()),
                }]
            })
        );
        assert_eq!(
            decode_event(r#"{"PlayerSnapshot":{"subject":"s","via":"v","snapshot":null}}"#),
            Some(ModEvent::PlayerSnapshot {
                subject: "s".into(),
                via: "v".into(),
                snapshot: Box::new(None),
            })
        );
        assert_eq!(
            decode_event(
                r#"{"PlayerSnapshot":{"subject":"s","via":"v","snapshot":{"did":"d","rkey":"r","at_uri":"u","version":2,"issued_at":"i","username":"M","dimension":"dim","x":1,"y":2,"z":3,"health":"20","food":20,"xp_level":0,"game_mode":"survival","verdict":{"Verified":{"endorser":"k"}}}}}"#
            ),
            Some(ModEvent::PlayerSnapshot {
                subject: "s".into(),
                via: "v".into(),
                snapshot: Box::new(Some(PlayerDigest {
                    did: "d".into(),
                    rkey: "r".into(),
                    at_uri: "u".into(),
                    version: 2,
                    issued_at: "i".into(),
                    username: "M".into(),
                    dimension: "dim".into(),
                    x: 1,
                    y: 2,
                    z: 3,
                    health: "20".into(),
                    food: 20,
                    xp_level: 0,
                    game_mode: "survival".into(),
                    verdict: EndorsementVerdict::Verified {
                        endorser: "k".into()
                    },
                })),
            })
        );
        assert_eq!(
            decode_event(
                r#"{"Profile":{"subject":"d","via":"v","profile":{"did":"d","handle":"alice.test","display_name":"Alice","description":"bio","pronouns":"they/them","avatar":"a"}}}"#
            ),
            Some(ModEvent::Profile {
                subject: "d".into(),
                via: "v".into(),
                profile: Box::new(ProfileDigest {
                    did: "d".into(),
                    handle: "alice.test".into(),
                    display_name: "Alice".into(),
                    description: "bio".into(),
                    pronouns: Some("they/them".into()),
                    avatar: "a".into(),
                }),
            })
        );
        assert_eq!(
            decode_event(
                r#"{"PlayerSkin":{"subject":"s","via":"v","skin":{"did":"d","rkey":"r","at_uri":"u","model":"slim","timestamp":"t","verdict":"BadSignature","skin":{"cid":"c","mime_type":"image/png","size":5,"path":null},"cape":null}}}"#
            ),
            Some(ModEvent::PlayerSkin {
                subject: "s".into(),
                via: "v".into(),
                skin: Box::new(Some(SkinDigest {
                    did: "d".into(),
                    rkey: "r".into(),
                    at_uri: "u".into(),
                    model: "slim".into(),
                    timestamp: "t".into(),
                    verdict: EndorsementVerdict::BadSignature,
                    skin: Some(BlobDigest {
                        cid: "c".into(),
                        mime_type: "image/png".into(),
                        size: 5,
                        path: None,
                    }),
                    cape: None,
                })),
            })
        );
        assert_eq!(
            decode_event(r#"{"ServerProfile":{"subject":"s","via":"v","profile":null}}"#),
            Some(ModEvent::ServerProfile {
                subject: "s".into(),
                via: "v".into(),
                profile: Box::new(None),
            })
        );
        assert_eq!(
            decode_event(
                r#"{"ServerProfile":{"subject":"s","via":"v","profile":{"did":"d","rkey":"r","at_uri":"u","name":"n","description":"desc","address":"addr","minecraft_version":"mc","atpcraft_version":"atp","capabilities":["cap"],"operator_handle":"op","created_at":"c","updated_at":"up","signing_key_id":"k","verdict":{"Trusted":{"key_id":"k"}}}}}"#
            ),
            Some(ModEvent::ServerProfile {
                subject: "s".into(),
                via: "v".into(),
                profile: Box::new(Some(ServerDigest {
                    did: "d".into(),
                    rkey: "r".into(),
                    at_uri: "u".into(),
                    name: "n".into(),
                    description: "desc".into(),
                    address: "addr".into(),
                    minecraft_version: "mc".into(),
                    atpcraft_version: "atp".into(),
                    capabilities: vec!["cap".into()],
                    operator_handle: "op".into(),
                    created_at: Some("c".into()),
                    updated_at: "up".into(),
                    signing_key_id: "k".into(),
                    verdict: ServerVerdict::Trusted { key_id: "k".into() },
                })),
            })
        );
        // Malformed lines decode to None (the caller reports a Failed event).
        for bad in [
            "",
            "not json",
            "{}",
            "[]",
            r#"{"Unknown":{}}"#,
            r#"{"Posted":{}}"#,
            r#"{"GateDecision":{"list":"l","subject":"s","allowed":"yes"}}"#,
            r#"{"Session":"Maybe"}"#,
            r#"{"PlayerSnapshot":{"subject":"s","via":"v","snapshot":{"did":"d"}}}"#,
            r#"{"ServerProfile":{"subject":"s","via":"v","profile":{"did":"d"}}}"#,
            "{\"a\":1} trailing",
        ] {
            assert_eq!(decode_event(bad), None, "line: {bad:?}");
        }
    }

    #[test]
    fn json_parser_handles_escapes_and_rejects_garbage() {
        assert_eq!(
            parse_json(r#""A\nb\t\"\\/""#),
            Some(Json::Str("A\nb\t\"\\/".into()))
        );
        assert_eq!(parse_json(r#""\u0041""#), Some(Json::Str("A".into())));
        // A surrogate pair: U+1F600.
        assert_eq!(
            parse_json(r#""\ud83d\ude00""#),
            Some(Json::Str("\u{1F600}".into()))
        );
        assert_eq!(
            parse_json(r#"{"a":[1,-2,null,true,false,{"b":"c"}]}"#),
            Some(jobj(vec![(
                "a",
                Json::Arr(vec![
                    Json::Int(1),
                    Json::Int(-2),
                    Json::Null,
                    Json::Bool(true),
                    Json::Bool(false),
                    jobj(vec![("b", jstr("c"))]),
                ])
            )]))
        );
        // Whitespace is fine; trailing garbage and lone surrogates are not.
        assert_eq!(
            parse_json("  {\"a\" : 1 }  "),
            Some(jobj(vec![("a", Json::Int(1))]))
        );
        assert_eq!(parse_json(""), None);
        assert_eq!(parse_json("{\"a\":1} x"), None);
        assert_eq!(parse_json(r#""\ud83d""#), None);
        assert_eq!(parse_json(r#""\ud83dx""#), None);
        assert_eq!(parse_json("1.5"), None, "the wire format has no floats");
        assert_eq!(parse_json("[1,"), None);
    }

    /// The full spawn path against a fake worker script: commands go down the
    /// child's stdin as JSON lines, the child's stdout lines come back as
    /// events, and Shutdown ends both.
    #[cfg(unix)]
    #[test]
    fn sidecar_bridge_forwards_both_directions() {
        use std::os::unix::fs::PermissionsExt;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "atmospheric-sidecar-rt-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-worker");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             printf '%s\\n' '{\"Session\":\"LoggedOut\"}'\n\
             while IFS= read -r line; do\n\
             case \"$line\" in\n\
             *Shutdown*) exit 0 ;;\n\
             *) printf '%s\\n' \"{\\\"Failed\\\":{\\\"message\\\":\\\"got:$line\\\"}}\" ;;\n\
             esac\n\
             done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let data_dir = dir.join("data");

        let (tx, rx) = spawn_sidecar(&script, &data_dir).expect("spawn the fake worker");
        let timeout = std::time::Duration::from_secs(5);

        // The handshake event arrives first.
        assert_eq!(
            rx.recv_timeout(timeout),
            Ok(ModEvent::Session(SessionState::LoggedOut))
        );
        // A command goes down as one exact JSON line (echoed by the fake).
        tx.send(GameCommand::PostText { text: "hi".into() })
            .unwrap();
        match rx.recv_timeout(timeout) {
            Ok(ModEvent::Failed { message }) => {
                assert!(
                    message.contains(r#"{"PostText":{"text":"hi"}}"#),
                    "the fake worker echoed: {message}"
                );
            }
            other => panic!("expected the echo Failed event, got {other:?}"),
        }
        // Shutdown is forwarded; the fake exits, stdout closes, the channel ends.
        tx.send(GameCommand::Shutdown).unwrap();
        assert!(rx.recv_timeout(timeout).is_err());
        drop(tx);
    }
}
