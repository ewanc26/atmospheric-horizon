//! Continuity Watchman (#190): DID-keyed identity observations for the
//! optional sidecar.
//!
//! The PLC export stream and a jittered resolve poll feed one bounded, in-memory
//! snapshot/event log. This layer is additive: it never changes link claims,
//! join gates, or the game/worker control plane.

mod feed;
mod model;
mod store;
mod watch;

pub use model::{
    ContinuitySettings, ContinuityStatus, DEFAULT_MAX_EVENTS, DEFAULT_POLL_SECS, ExportOperation,
    IdentityChange, IdentityEvent, IdentitySnapshot, MAX_JITTER_SECS, MAX_POLL_SECS, MIN_POLL_SECS,
    WatchedIdentity, diff_snapshots, export_page_url, export_stream_url, parse_export_line,
    snapshot_from_did_document, snapshot_from_operation, validate_watch_did,
};
pub use store::Continuity;
pub use watch::Watcher;
