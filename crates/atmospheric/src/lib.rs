//! AT Protocol client logic for the atmospheric Horizon mod.
//!
//! Everything in this crate builds and tests without a game: no Horizon
//! imports, no mixin declarations, only the protocol side. The game side (the
//! injected `runtime` module, kept std-only) talks to this crate through the
//! command/event channels owned by [`worker`], which is the only place that
//! performs network or disk I/O.

pub mod actions;
pub mod config;
pub mod error;
pub mod notifications;
pub mod records;
pub mod session;
pub mod signing;
pub mod tid;
pub mod worker;

pub use config::Config;
pub use error::AtmosphericError;

/// What the game side asks the worker thread to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Log in with the configured credentials (app-password flow).
    Login,
    /// Replace the settings file with these settings.
    SaveConfig { config: Config },
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
    /// Publish the player's state as an `at.atpcraft.player.snapshot` record,
    /// endorsed with the mod's own Ed25519 key (atpcraft port, M6). An empty
    /// `summary.username` is filled from the configured identifier.
    PostSnapshot { summary: records::SnapshotSummary },
    /// Post the player's death message to Bluesky (only when
    /// `post_death_messages` is enabled).
    PostDeathMessage { text: String },
    /// Log out: drop the session and delete the persisted refresh token.
    Logout,
    /// Shut the worker thread down.
    Shutdown,
}

/// What the worker thread reports back to the game side.
#[derive(Debug, Clone)]
pub enum Event {
    /// Session health, mirrored on the HUD.
    Session(session::SessionState),
    /// A record was written; carries its `at://` URI.
    Posted { uri: String },
    /// Something failed; carries a HUD-friendly message.
    Failed { message: String },
    /// The latest notification digest for the HUD panel.
    Notifications {
        items: Vec<notifications::NotificationDigest>,
    },
}
