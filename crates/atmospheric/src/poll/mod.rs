//! Counting Stones (#193): verifiable in-game polls with a server-signed
//! Merkle-root tally. The protocol side builds and tests without a game.

pub mod ballot;
pub mod digest;
pub mod record;
pub mod tree;
