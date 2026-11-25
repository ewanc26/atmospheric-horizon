//! Ledger of Decisions (#199): signed, privacy-preserving moderation records.
//!
//! This scope is pure protocol logic: it builds and verifies records and
//! resolves their current state without a game, a PDS or a filesystem. The
//! sidecar owns persistence and publishing; the game half owns mod actions.

pub mod record;
pub mod subject;
