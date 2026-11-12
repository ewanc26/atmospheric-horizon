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
}

/// Commands the game side sends to the worker thread.
// The worker side is not linked into the game build yet (see
// docs/ARCHITECTURE.md), so only `Login`/`PostText` are constructed today.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum GameCommand {
    /// Log in with the configured credentials (app-password flow).
    Login,
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
    /// Stop the worker thread.
    Shutdown,
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
    /// The latest notification digest (M4).
    Notifications { items: Vec<NotificationDigest> },
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
static STATUS: OnceLock<Mutex<String>> = OnceLock::new();
static LAST_SHOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn status_slot() -> &'static Mutex<String> {
    // Empty until the first worker event: with no worker attached (see
    // issue #2) there is nothing worth showing, so the HUD box stays hidden.
    STATUS.get_or_init(|| Mutex::new(String::new()))
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
        for event in &drained {
            *status = status_text(event);
        }
    }
    drained
}

/// The current HUD status line; empty until the first worker event, in which
/// case the HUD draws nothing.
pub fn status_line() -> String {
    status_slot()
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default()
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
        // No worker event yet: the HUD line is empty, so nothing is drawn.
        assert!(status_line().is_empty());

        // Game side -> worker: the commands the hooks send.
        login();
        post_text("hello from the game");
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
        });
        shutdown();
        assert!(matches!(
            command_rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(GameCommand::Login)
        ));
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::PostText { text }) => assert_eq!(text, "hello from the game"),
            other => panic!("expected PostText, got {:?}", other),
        }
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
    }

    #[test]
    fn location_line_reads_like_where() {
        assert_eq!(
            location_line(12.5, 64.0, -3.25, "minecraft:the_nether"),
            "At 12, 64, -4 in minecraft:the_nether — from Horizon"
        );
    }
}
