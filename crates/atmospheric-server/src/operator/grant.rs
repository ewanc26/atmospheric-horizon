//! Short-lived, scoped admin grants minted after a PDS-custody proof (#189).

use super::model::{GrantScope, OperatorSettings};
use crate::{error::ApiError, oauth::random_token};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// A minted grant. The bearer token is the map key and is never copied into
/// the value, so an audit receipt cannot leak a live credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub id: String,
    pub did: String,
    pub scopes: Vec<GrantScope>,
    pub issued_at: u64,
    pub expires_at: u64,
}

impl Grant {
    pub fn is_current(&self, now: u64) -> bool {
        now >= self.issued_at && now < self.expires_at
    }

    pub fn allows(&self, scope: GrantScope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// Result of presenting a bearer token to a route that needs a scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantCheck {
    /// Token is a live grant carrying the required scope.
    Authorized(Grant),
    /// Token is a live grant, but it was minted without the required scope.
    ScopeDenied(Grant),
    /// Token is not a live grant; the static admin token may still apply.
    Unknown,
}

/// In-memory grants. A restart invalidates every grant, matching the browser
/// link and skin upload token behaviour. Key compromise therefore has a short
/// maximum lifetime, bounded by [`super::model::MAX_GRANT_TTL_SECS`].
#[derive(Clone, Debug, Default)]
pub struct Grants {
    tokens: Arc<Mutex<HashMap<String, Grant>>>,
}

impl Grants {
    /// Mint a grant for an already verified operator DID.
    pub fn issue(
        &self,
        settings: &OperatorSettings,
        did: &str,
        now: u64,
    ) -> Result<(String, Grant), ApiError> {
        let mut tokens = self.tokens.lock().map_err(|_| ApiError::Internal)?;
        let token = loop {
            let candidate = random_token(32);
            if !tokens.contains_key(&candidate) {
                break candidate;
            }
        };
        let grant = Grant {
            id: random_token(12),
            did: did.to_owned(),
            scopes: settings.scopes.clone(),
            issued_at: now,
            expires_at: now.saturating_add(settings.grant_ttl_secs),
        };
        tokens.insert(token.clone(), grant.clone());
        Ok((token, grant))
    }

    /// Check a bearer token against a required scope. Expired grants are
    /// removed while checking. The caller logs both successful and denied
    /// uses, because both are exercises of a live grant.
    pub fn check(&self, token: &str, scope: GrantScope, now: u64) -> GrantCheck {
        let Ok(mut tokens) = self.tokens.lock() else {
            return GrantCheck::Unknown;
        };
        tokens.retain(|_, grant| grant.is_current(now));
        match tokens.get(token) {
            Some(grant) if grant.allows(scope) => GrantCheck::Authorized(grant.clone()),
            Some(grant) => GrantCheck::ScopeDenied(grant.clone()),
            None => GrantCheck::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_are_scoped_and_expire() {
        let grants = Grants::default();
        let settings = OperatorSettings {
            scopes: vec![GrantScope::LinksWrite],
            grant_ttl_secs: 60,
            ..OperatorSettings::default()
        };
        let (token, grant) = grants.issue(&settings, "did:plc:op", 1_000).unwrap();
        assert!(grant.allows(GrantScope::LinksWrite));
        assert!(!grant.allows(GrantScope::AuditRead));
        assert_eq!(
            grants.check(&token, GrantScope::LinksWrite, 1_001),
            GrantCheck::Authorized(grant.clone())
        );
        assert_eq!(
            grants.check(&token, GrantScope::AuditRead, 1_001),
            GrantCheck::ScopeDenied(grant.clone())
        );
        assert_eq!(
            grants.check(&token, GrantScope::LinksWrite, 1_060),
            GrantCheck::Unknown
        );
    }

    #[test]
    fn grants_do_not_expose_their_token() {
        let grants = Grants::default();
        let (token, _) = grants
            .issue(
                &OperatorSettings::default(),
                "did:plc:op",
                super::super::challenge::now_secs(),
            )
            .unwrap();
        let stored = grants.tokens.lock().unwrap();
        assert!(
            stored
                .values()
                .all(|grant| !format!("{grant:?}").contains(&token))
        );
    }
}
