//! Stand-In (#206): player-owned, scoped delegation over Horizon's agent-control surface.
//!
//! This scope is the local-deny path: a pinned catalog of `horizon-ctl` command
//! names, deny-by-default authorization, single-use challenge nonces, and
//! signed grant/challenge/receipt records. It is pure protocol logic: no game,
//! socket, PDS or filesystem.
//!
//! The doorkeeper daemon that wraps the player's local control socket, the
//! in-game grant screen and publication of records to the player's PDS remain
//! native work. This module never executes a command and never opens a socket.

pub mod challenge;
pub mod command;
pub mod grant;
pub mod nonce;
