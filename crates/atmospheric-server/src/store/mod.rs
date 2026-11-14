//! Durable link and pending-pairing storage.
//!
//! The state is a small JSON document rewritten atomically on every mutation.
//! Writes are serialised by an async lock, and a failed write leaves the
//! in-memory state untouched. The service is designed for one process per data
//! directory; run a shared database if you need multiple replicas.

mod file;
mod link;

pub use file::default_links_path;
pub use link::LinkStore;
