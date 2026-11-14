//! Foundation atoms shared by the protocol, social and worker scopes.
//!
//! The modules are re-exported at the crate root to preserve the existing
//! `atmospheric::config`, `error`, `records`, `signing` and `tid` paths.

pub mod config;
pub mod error;
pub mod records;
pub mod signing;
pub mod tid;
