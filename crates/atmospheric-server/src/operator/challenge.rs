//! One-time, expiring Proof-of-Handle challenges for operator login (#189).

use crate::error::ApiError;
use atmospheric::proof::Challenge;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use super::model::CHALLENGE_TTL_SECS;

/// In-memory pending challenges, keyed by DID. Restart invalidates them, just
/// like the browser link and skin upload tokens.
#[derive(Clone, Debug, Default)]
pub struct Challenges {
    pending: Arc<Mutex<HashMap<String, Challenge>>>,
}

impl Challenges {
    /// Issue (or replace) the challenge for a DID and return it.
    pub fn issue(&self, did: &str) -> Result<Challenge, ApiError> {
        let challenge = Challenge::fresh()
            .map_err(|error| ApiError::Invalid(format!("could not create challenge: {error}")))?;
        self.pending
            .lock()
            .map_err(|_| ApiError::Internal)?
            .insert(did.to_owned(), challenge.clone());
        Ok(challenge)
    }

    /// Redeem a challenge, consuming it even when verification later fails.
    /// A failed hand-off must not leave a reusable nonce lying around.
    pub fn redeem(&self, did: &str, nonce: &str, now: u64) -> Result<Challenge, ApiError> {
        let mut pending = self.pending.lock().map_err(|_| ApiError::Internal)?;
        let challenge = pending.remove(did).ok_or(ApiError::ChallengeUnavailable)?;
        if challenge.nonce != nonce {
            return Err(ApiError::ChallengeUnavailable);
        }
        if now < challenge.issued_at || now - challenge.issued_at > CHALLENGE_TTL_SECS {
            return Err(ApiError::ChallengeUnavailable);
        }
        Ok(challenge)
    }

    /// Drop expired challenges. Called on issue to bound memory growth.
    pub fn prune(&self, now: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|_, challenge| {
                now >= challenge.issued_at && now - challenge.issued_at <= CHALLENGE_TTL_SECS
            });
        }
    }
}

/// Current Unix time, or 0 if the clock is before the epoch.
pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_one_time_and_fresh() {
        let store = Challenges::default();
        let challenge = store.issue("did:plc:op").unwrap();
        let now = challenge.issued_at + 10;
        assert_eq!(
            store.redeem("did:plc:op", &challenge.nonce, now).unwrap(),
            challenge
        );
        assert!(matches!(
            store.redeem("did:plc:op", &challenge.nonce, now),
            Err(ApiError::ChallengeUnavailable)
        ));
    }

    #[test]
    fn challenge_rejects_wrong_nonce_and_expiry() {
        let store = Challenges::default();
        let challenge = store.issue("did:plc:op").unwrap();
        assert!(matches!(
            store.redeem("did:plc:op", "wrong", challenge.issued_at),
            Err(ApiError::ChallengeUnavailable)
        ));
        let late = challenge.issued_at + CHALLENGE_TTL_SECS + 1;
        assert!(matches!(
            store.redeem("did:plc:op", &challenge.nonce, late),
            Err(ApiError::ChallengeUnavailable)
        ));
    }
}
