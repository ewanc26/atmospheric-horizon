//! Operator's Warrant (#189): DID-authenticated admin grants.
//!
//! This is deliberately additive over the stable control API: the static admin
//! token keeps working, and allowlisted operators can instead prove PDS custody
//! with the Proof-of-Handle chain (#185). The service then mints a short-lived
//! scoped grant that is accepted on admin operations under this repo's `/v1`
//! API. Every exercised grant writes a signed audit receipt.
//!
//! This layer is experimental and stays inside the sidecar process boundary:
//! it does not change the loopback/token-gated socket assumption of the game's
//! own control plane, and no player-facing gate reads any of this state.

mod audit;
mod challenge;
mod grant;
mod model;
mod verify;

use crate::error::ApiError;
use std::{fmt, path::Path};

pub use audit::{AuditLog, Receipt};
pub use challenge::Challenges;
pub use grant::{Grant, GrantCheck, Grants};
pub use model::{CHALLENGE_TTL_SECS, DEFAULT_GRANT_TTL_SECS, GrantScope, OperatorSettings};
pub use verify::{VerifyFn, VerifyFuture, public_verify};

/// Runtime state for DID-authenticated operator access.
#[derive(Clone)]
pub struct Operator {
    pub(crate) settings: OperatorSettings,
    pub(crate) challenges: Challenges,
    pub(crate) grants: Grants,
    pub(crate) audit: AuditLog,
    pub(crate) verify: VerifyFn,
}

impl Operator {
    /// Validate the policy and open its owner-only audit log. The caller
    /// decides whether an empty allowlist means "leave the feature off".
    pub fn open(
        directory: impl AsRef<Path>,
        settings: OperatorSettings,
        verify: VerifyFn,
    ) -> Result<Self, ApiError> {
        settings.validate().map_err(ApiError::Invalid)?;
        Ok(Self {
            settings,
            challenges: Challenges::default(),
            grants: Grants::default(),
            audit: AuditLog::open(directory)?,
            verify,
        })
    }

    /// Stable id of the key that signs audit receipts.
    pub fn audit_key_id(&self) -> &str {
        self.audit.key_id()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.settings.is_enabled()
    }

    /// Issue a fresh challenge for an allowlisted DID.
    pub(crate) fn challenge(&self, did: &str) -> Result<atmospheric::proof::Challenge, ApiError> {
        if !self.enabled() || !self.settings.allows(did) {
            return Err(ApiError::Forbidden(
                "DID is not an allowlisted operator".into(),
            ));
        }
        let now = challenge::now_secs();
        self.challenges.prune(now);
        self.challenges.issue(did)
    }

    /// Redeem a challenge, verify the PDS-custody proof, and mint a grant.
    pub(crate) async fn grant(&self, did: &str, nonce: &str) -> Result<(String, Grant), ApiError> {
        if !self.enabled() || !self.settings.allows(did) {
            return Err(ApiError::Forbidden(
                "DID is not an allowlisted operator".into(),
            ));
        }
        let now = challenge::now_secs();
        let challenge = self.challenges.redeem(did, nonce, now)?;
        let proof = (self.verify)(did.to_owned(), challenge)
            .await
            .map_err(|message| ApiError::Invalid(format!("operator proof failed: {message}")))?;
        if proof.did != did {
            return Err(ApiError::Unauthorized);
        }
        self.grants.issue(&self.settings, &proof.did, now)
    }
}

impl fmt::Debug for Operator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Operator")
            .field("settings", &self.settings)
            .field("audit_key_id", &self.audit.key_id())
            .finish_non_exhaustive()
    }
}
