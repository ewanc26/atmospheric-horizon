//! In-memory one-time state for browser linking.
//!
//! A link token is created by the game server (authenticated with the shared
//! link token) and shown only to that player. OAuth pending state lives for ten
//! minutes and is consumed once. Nothing here is persisted: a service restart
//! invalidates browser links, which is safer than persisting DPoP private keys.

mod state;

pub use state::{BROWSER_TTL_SECS, BrowserLinks, PendingLink};
