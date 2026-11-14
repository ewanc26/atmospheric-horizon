//! Background worker thread: the only place network and disk I/O happens.
//!
//! The service atom owns the tokio runtime, the AT Protocol agent and the
//! settings files. This scope root keeps the existing public
//! `atmospheric::worker` path stable.

mod service;

pub use service::*;
