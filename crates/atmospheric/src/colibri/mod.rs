//! Colibri channel, record, membership and event-stream integration.
//!
//! The implementation lives in the `bridge` atom; this scope root keeps the
//! existing public `atmospheric::colibri` path stable.

mod bridge;

pub use bridge::*;
