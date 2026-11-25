//! Salted, hash-reduced moderation subjects (#199).
//!
//! A public decision record never carries a plaintext handle or DID. Instead it
//! carries a 64-character hex identifier derived from the subject DID and a
//! server-held pepper. The pepper is secret, so an outsider who walks the
//! collection cannot dictionary-attack a small set of known DIDs into the
//! published identifiers. The server can recompute an identifier for a subject
//! it knows, and can hand that subject their own identifier over a private
//! channel later; neither is possible for a reader of the public collection.

use crate::poll::tree::{hash_parts, hex};
use getrandom::{SysRng, rand_core::Rng, rand_core::UnwrapErr};

/// Domain separator for moderation subject identifiers. Kept distinct from the
/// poll commitment and nullifier domains so a digest cannot be reinterpreted.
const DOMAIN_SUBJECT: &[u8] = b"atmospheric-ledger/subject/v1";

/// Derive the public identifier for a subject DID under a server pepper.
pub fn identifier(pepper: &[u8; 32], did: &str) -> String {
    hex(&hash_parts(DOMAIN_SUBJECT, &[pepper, did.as_bytes()]))
}

/// Draw a fresh 32-byte pepper from the OS randomness source. Used once, when
/// the sidecar creates a new ledger and persists the pepper owner-only.
pub fn fresh_pepper() -> [u8; 32] {
    let mut pepper = [0u8; 32];
    UnwrapErr(SysRng).fill_bytes(&mut pepper);
    pepper
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_is_stable_and_salted() {
        let pepper = [7u8; 32];
        let one = identifier(&pepper, "did:plc:alice");
        let two = identifier(&pepper, "did:plc:alice");
        assert_eq!(one, two);
        assert_eq!(one.len(), 64);
        assert!(
            one.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_ne!(one, identifier(&pepper, "did:plc:bob"));
        assert_ne!(one, identifier(&[8u8; 32], "did:plc:alice"));
    }

    #[test]
    fn fresh_pepper_is_not_all_zero() {
        assert_ne!(fresh_pepper(), [0u8; 32]);
    }
}
