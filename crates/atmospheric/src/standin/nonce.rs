//! Single-use challenge nonces for the Stand-In doorkeeper (#206).
//!
//! The doorkeeper issues a nonce, the agent signs a challenge over it, and the
//! doorkeeper consumes it on the first presentation — allowed or denied. A
//! second presentation of the same nonce is a replay. Expiry runs on the
//! doorkeeper's own issue time, never on timestamps the agent signed. The
//! book is in memory only and bounded: when it is full the oldest nonce is
//! forgotten, which fails closed (that nonce can no longer be redeemed).

use std::collections::VecDeque;

use crate::error::Result;

use super::challenge::{MAX_CHALLENGE_TTL_SECS, fresh_nonce};

/// How long an issued nonce stays redeemable, in seconds.
pub const NONCE_TTL_SECS: i64 = MAX_CHALLENGE_TTL_SECS;
/// Most nonces outstanding at once.
pub const MAX_OUTSTANDING: usize = 256;

/// Outstanding nonces with the instant the doorkeeper issued each one.
#[derive(Debug, Default)]
pub struct NonceBook {
    outstanding: VecDeque<(String, i64)>,
}

impl NonceBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh nonce at `now` (Unix seconds).
    pub fn issue(&mut self, now: i64) -> Result<String> {
        let nonce = fresh_nonce()?;
        self.prune(now);
        while self.outstanding.len() >= MAX_OUTSTANDING {
            self.outstanding.pop_front();
        }
        self.outstanding.push_back((nonce.clone(), now));
        Ok(nonce)
    }

    /// Redeem `nonce` at `now`. It is removed whether or not it is still
    /// fresh, so it can never be redeemed twice. Returns `true` only for an
    /// outstanding nonce issued no more than [`NONCE_TTL_SECS`] ago.
    pub fn consume(&mut self, nonce: &str, now: i64) -> bool {
        let Some(index) = self
            .outstanding
            .iter()
            .position(|(issued, _)| issued == nonce)
        else {
            return false;
        };
        let (_, issued_at) = self
            .outstanding
            .remove(index)
            .expect("the index was just found");
        issued_at <= now && now < issued_at + NONCE_TTL_SECS
    }

    /// Number of nonces still held.
    pub fn len(&self) -> usize {
        self.outstanding.len()
    }

    pub fn is_empty(&self) -> bool {
        self.outstanding.is_empty()
    }

    fn prune(&mut self, now: i64) {
        self.outstanding
            .retain(|(_, issued_at)| now < issued_at + NONCE_TTL_SECS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standin::challenge::is_nonce;

    const NOW: i64 = 1_795_000_000;

    #[test]
    fn a_nonce_redeems_exactly_once() {
        let mut book = NonceBook::new();
        let nonce = book.issue(NOW).unwrap();
        assert!(is_nonce(&nonce));
        assert!(book.consume(&nonce, NOW + 1));
        assert!(!book.consume(&nonce, NOW + 2), "replay must fail");
        assert!(book.is_empty());
    }

    #[test]
    fn unknown_nonces_are_rejected() {
        let mut book = NonceBook::new();
        book.issue(NOW).unwrap();
        assert!(!book.consume(&"0".repeat(32), NOW));
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn expired_nonces_are_burned_not_redeemed() {
        let mut book = NonceBook::new();
        let nonce = book.issue(NOW).unwrap();
        assert!(!book.consume(&nonce, NOW + NONCE_TTL_SECS));
        assert!(book.is_empty());
        let early = book.issue(NOW).unwrap();
        assert!(!book.consume(&early, NOW - 1), "clock went backwards");
    }

    #[test]
    fn issuing_prunes_expired_and_caps_the_book() {
        let mut book = NonceBook::new();
        let stale = book.issue(NOW).unwrap();
        book.issue(NOW + NONCE_TTL_SECS).unwrap();
        assert_eq!(book.len(), 1);
        assert!(!book.consume(&stale, NOW + NONCE_TTL_SECS));

        let mut book = NonceBook::new();
        let first = book.issue(NOW).unwrap();
        for _ in 1..MAX_OUTSTANDING {
            book.issue(NOW).unwrap();
        }
        assert_eq!(book.len(), MAX_OUTSTANDING);
        let last = book.issue(NOW).unwrap();
        assert_eq!(book.len(), MAX_OUTSTANDING);
        assert!(!book.consume(&first, NOW), "the oldest nonce was evicted");
        assert!(book.consume(&last, NOW));
    }
}
