//! Configuration and value types for DID-authenticated operator grants (#189).

use serde::{Deserialize, Serialize};

/// Default validity window of a minted grant (15 minutes).
pub const DEFAULT_GRANT_TTL_SECS: u64 = 15 * 60;
/// Upper bound on a grant's validity window. Key compromise is server takeover,
/// so a long-lived grant is refused rather than silently clamped.
pub const MAX_GRANT_TTL_SECS: u64 = 60 * 60;
/// A challenge may be redeemed for this long after issue.
pub const CHALLENGE_TTL_SECS: u64 = 5 * 60;

/// A single admin capability a grant may carry. Grants are scoped: a token
/// minted for `links_write` cannot read the audit log or emit a label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    LinksRead,
    LinksWrite,
    PairingCreate,
    LabelEmit,
    LedgerWrite,
    AuditRead,
}

impl std::fmt::Display for GrantScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for GrantScope {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "links_read" => Ok(Self::LinksRead),
            "links_write" => Ok(Self::LinksWrite),
            "pairing_create" => Ok(Self::PairingCreate),
            "label_emit" => Ok(Self::LabelEmit),
            "ledger_write" => Ok(Self::LedgerWrite),
            "audit_read" => Ok(Self::AuditRead),
            other => Err(format!("unknown operator grant scope: {other:?}")),
        }
    }
}

impl GrantScope {
    /// Stable wire name, used in config and audit receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinksRead => "links_read",
            Self::LinksWrite => "links_write",
            Self::PairingCreate => "pairing_create",
            Self::LabelEmit => "label_emit",
            Self::LedgerWrite => "ledger_write",
            Self::AuditRead => "audit_read",
        }
    }
}

/// Operator allowlist and grant policy, read from the environment at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorSettings {
    /// DIDs allowed to exchange a Proof-of-Handle for a grant.
    pub dids: Vec<String>,
    /// Lifetime of a minted grant in seconds.
    pub grant_ttl_secs: u64,
    /// Capabilities every minted grant carries.
    pub scopes: Vec<GrantScope>,
}

impl Default for OperatorSettings {
    fn default() -> Self {
        Self {
            dids: Vec::new(),
            grant_ttl_secs: DEFAULT_GRANT_TTL_SECS,
            scopes: GrantScope::all(),
        }
    }
}

impl OperatorSettings {
    /// True when DID authentication is enabled at all.
    pub fn is_enabled(&self) -> bool {
        !self.dids.is_empty()
    }

    /// Whether a DID is on the allowlist. DIDs are compared byte-for-byte.
    pub fn allows(&self, did: &str) -> bool {
        self.dids.iter().any(|allowed| allowed == did)
    }

    /// Validate the policy before it is installed. An empty allowlist is fine
    /// (the feature is simply off); a nonempty one must use supported DID
    /// methods and a bounded lifetime.
    pub fn validate(&self) -> Result<(), String> {
        for did in &self.dids {
            if !did.starts_with("did:plc:") && !did.starts_with("did:web:") {
                return Err(format!(
                    "operator DID {did:?} is not a did:plc or did:web identifier"
                ));
            }
        }
        if self.grant_ttl_secs == 0 || self.grant_ttl_secs > MAX_GRANT_TTL_SECS {
            return Err(format!(
                "operator grant lifetime must be between 1 and {MAX_GRANT_TTL_SECS} seconds"
            ));
        }
        if self.scopes.is_empty() {
            return Err("operator grant needs at least one scope".into());
        }
        Ok(())
    }
}

impl GrantScope {
    /// Every capability, used as the default grant policy.
    pub fn all() -> Vec<Self> {
        vec![
            Self::LinksRead,
            Self::LinksWrite,
            Self::PairingCreate,
            Self::LabelEmit,
            Self::LedgerWrite,
            Self::AuditRead,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_validates_dids_and_lifetime() {
        let mut settings = OperatorSettings::default();
        settings.validate().unwrap(); // disabled is valid
        settings.dids.push("did:plc:operator".into());
        settings.validate().unwrap();
        settings.dids.push("did:key:nope".into());
        assert!(settings.validate().is_err());
        settings.dids.pop();
        settings.grant_ttl_secs = MAX_GRANT_TTL_SECS + 1;
        assert!(settings.validate().is_err());
        settings.grant_ttl_secs = DEFAULT_GRANT_TTL_SECS;
        settings.scopes.clear();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn scopes_have_stable_wire_names() {
        assert_eq!(GrantScope::LinksWrite.as_str(), "links_write");
        assert_eq!(
            serde_json::to_string(&GrantScope::LabelEmit).unwrap(),
            r#""label_emit""#
        );
    }
}
