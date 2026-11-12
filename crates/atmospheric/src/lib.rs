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
pub mod session;
pub mod worker;

pub use config::Config;
pub use error::AtmosphericError;

/// What the game side asks the worker thread to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Log in with the configured credentials (app-password flow).
    Login,
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
