//! Account identity, authentication and persisted sessions.
//!
//! `identity`, `oauth` and `session` are re-exported at the crate root, so
//! their public paths remain `atmospheric::identity`, `atmospheric::oauth`
//! and `atmospheric::session`.

pub mod identity;
pub mod oauth;
pub mod session;
