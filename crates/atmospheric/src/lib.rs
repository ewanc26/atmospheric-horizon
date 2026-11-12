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
pub mod labels;
pub mod lists;
pub mod notifications;
pub mod oauth;
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
    /// Log in with localhost OAuth (M8): resolve the handle, open the browser
    /// at the authorization server, and exchange the loopback redirect.
    LoginOAuth { handle: String },
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
    /// Fetch the member DIDs of a public Bluesky list — the read side of
    /// atpcraft's join gate (M11). The reference is an
    /// `at://…/app.bsky.graph.list/<rkey>` URI or a bsky.app list URL.
    FetchList { reference: String },
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
    /// The labels found on a subject (M12, labeler read side): the subject
    /// (AT-URI or DID), where the labels came from (`via`: the AppView URL or
    /// the labeler's endpoint), and the labels themselves.
    Labels {
        subject: String,
        via: String,
        labels: Vec<labels::LabelDigest>,
    },
}
