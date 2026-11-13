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
pub const VERSION: &str = "0.1.0";
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
    /// Fetch a public Bluesky list's member DIDs (M11, join-gate read side):
    /// an `at://…app.bsky.graph.list/<rkey>` URI or a bsky.app list URL.
    FetchList { reference: String },
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
    /// Post the player's death message (only when `post_death_messages` is on).
    PostDeathMessage { text: String },
    /// Log out: drop the session and delete the persisted refresh token.
    Logout,
    /// Stop the worker thread.
    Shutdown,
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

/// The verdict of verifying a snapshot record's endorsements (M13). Plain
/// data: the worker side has the same enum in the `atmospheric` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotVerdict {
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
    pub verdict: SnapshotVerdict,
}

/// Events the worker thread reports back to the game side.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModEvent {
    /// A free-form status line.
    Status(String),
    /// Session health.
    Session(SessionState),
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
        ModEvent::Posted { uri } => format!("atmospheric: posted {uri}"),
        ModEvent::Failed { message } => format!("atmospheric: {message}"),
        ModEvent::OAuthAuthorize { url } => format!("atmospheric: finish login at {url}"),
        ModEvent::ListMembers { list, members } => {
            format!("atmospheric: list {list} has {} members", members.len())
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
                let verdict = match &d.verdict {
                    SnapshotVerdict::Verified { endorser } => format!("verified by {endorser}"),
                    SnapshotVerdict::UnknownEndorser => "unknown endorser".into(),
                    SnapshotVerdict::BadSignature => "BAD SIGNATURE".into(),
                    SnapshotVerdict::Malformed => "malformed record".into(),
                };
                format!(
                    "atmospheric: {} v{} in {} at {}, {}, {} — {verdict}",
                    d.username, d.version, d.dimension, d.x, d.y, d.z
                )
            }
        },
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

/// How many status lines the HUD shows at once.
pub const STATUS_LINES_MAX: usize = 3;

/// How long a HUD status line stays visible before it expires (#17).
pub const STATUS_LINE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn status_slot() -> &'static Mutex<Vec<(String, std::time::Instant)>> {
    // Empty until the first worker event: with no worker attached (see
    // issue #2) there is nothing worth showing, so the HUD box stays hidden.
    STATUS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Drop status lines older than [`STATUS_LINE_TTL`].
fn prune_expired(status: &mut Vec<(String, std::time::Instant)>, now: std::time::Instant) {
    status.retain(|(_, at)| now.duration_since(*at) < STATUS_LINE_TTL);
}

fn last_shot_slot() -> &'static Mutex<Option<String>> {
    LAST_SHOT.get_or_init(|| Mutex::new(None))
}

/// Create the channel pair and install the game side.
///
/// Returns the worker side — the receiving end of the commands and the
/// sending end of the events — exactly once. A second call returns `None`.
// Called by the worker glue once the bundle crate can be staged into the
// game build; until then the hooks only send.
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

/// Ask the worker to fetch a public Bluesky list's members (M11).
pub fn fetch_list(reference: &str) {
    send(GameCommand::FetchList {
        reference: reference.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
                    verdict: SnapshotVerdict::Verified {
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
                    verdict: SnapshotVerdict::BadSignature,
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
    }

    #[test]
    fn location_line_reads_like_where() {
        assert_eq!(
            location_line(12.5, 64.0, -3.25, "minecraft:the_nether"),
            "At 12, 64, -4 in minecraft:the_nether — from Horizon"
        );
    }
}
