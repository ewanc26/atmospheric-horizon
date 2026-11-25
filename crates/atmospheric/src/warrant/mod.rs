//! Warrants (#192): cosmetic ranks derived from signed credential records in
//! a player's own repo. An issuer signs a small `WarrantRecord`; the sidecar
//! accepts it only when the issuer DID, collection and Ed25519 key are pinned
//! in the worker's allowlist. Self-attested records are worthless and ignored.
//!
//! Warrants grant cosmetic name-plate lines only, never gameplay gating. A
//! deleted record simply disappears from the next fetch, so revocation is
//! "absence from the repo"; an `expiresAt` gives issuers a time-boxed grant.
//! The protocol side builds and tests without a game or a network.

pub mod digest;
pub mod record;
