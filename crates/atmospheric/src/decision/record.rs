//! Signed moderation decision records and their resolution (#199).
//!
//! Every record carries the collection NSID in its signed payload, so a
//! signature cannot be replayed under a different lexicon. Reversals and
//! expiries are signed records that name the rkey of the decision they end;
//! expiry by wall clock is additionally derivable from `expiresAt`. The public
//! subject is a salted identifier, never a handle or DID.

use crate::{
    card::digest::epoch_seconds,
    error::{AtmosphericError, Result},
    poll::tree,
    signing::{self, SigningKey},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Collection the ledger writes under the server account's repository.
pub const DECISION_COLLECTION: &str = "club.atmospheric.mod.decision";

/// Maximum length of a reason category, scope label and appeal pointer.
pub const MAX_CATEGORY_LEN: usize = 64;
pub const MAX_SCOPE_LEN: usize = 128;
pub const MAX_APPEAL_LEN: usize = 512;
/// Maximum length of an rkey referenced by a reversal or expiry.
pub const MAX_TARGET_LEN: usize = 64;

/// What a ledger entry asserts. `Reversal` and `Expiry` end the decision named
/// by `target`; they do not introduce a new moderation action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Decision,
    Reversal,
    Expiry,
}

impl DecisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Reversal => "reversal",
            Self::Expiry => "expiry",
        }
    }
}

impl std::str::FromStr for DecisionKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim() {
            "decision" => Ok(Self::Decision),
            "reversal" => Ok(Self::Reversal),
            "expiry" => Ok(Self::Expiry),
            other => Err(format!("unknown decision kind: {other:?}")),
        }
    }
}

/// A signed ledger record as published in the collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRecord {
    /// Signed to stop replay under another collection.
    pub collection: String,
    pub kind: DecisionKind,
    /// Salted, hash-reduced subject identifier; never a plaintext DID/handle.
    pub subject: String,
    /// Reason category, for example `spam` or `reversal`.
    pub category: String,
    /// Scope the decision applies to, for example `server` or a channel label.
    pub scope: String,
    /// rkey of the decision a reversal/expiry ends. Absent on decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// RFC 3339 UTC instant at which the decision stops applying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Optional pointer to an appeal thread or receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appeal: Option<String>,
    /// DID of the server account whose repository holds the record.
    pub issuer: String,
    /// RFC 3339 UTC creation time.
    pub created_at: String,
    /// SPKI base64 of the Ed25519 key that signed the record.
    pub signing_key: String,
    /// Base64 (no padding) Ed25519 signature over every other field.
    pub signature: String,
}

/// The signed subset, in a fixed field order shared by signer and verifier.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    collection: &'a str,
    kind: &'a str,
    subject: &'a str,
    category: &'a str,
    scope: &'a str,
    target: Option<&'a str>,
    expires_at: Option<&'a str>,
    appeal: Option<&'a str>,
    issuer: &'a str,
    created_at: &'a str,
    signing_key: &'a str,
}

impl DecisionRecord {
    /// The exact bytes the signature covers: every field except `signature`.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let payload = Payload {
            collection: &self.collection,
            kind: self.kind.as_str(),
            subject: &self.subject,
            category: &self.category,
            scope: &self.scope,
            target: self.target.as_deref(),
            expires_at: self.expires_at.as_deref(),
            appeal: self.appeal.as_deref(),
            issuer: &self.issuer,
            created_at: &self.created_at,
            signing_key: &self.signing_key,
        };
        // No maps, floats or custom serialisers: serialisation cannot fail.
        serde_json::to_vec(&payload).expect("the decision payload is always serialisable")
    }

    /// Fill in the issuer's public key and sign. Call after every other field
    /// is set; re-signing replaces any previous signature.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signing_key = signing::public_key_spki_b64(&key.verifying_key());
        self.signature = signing::sign_b64(key, &self.canonical_bytes());
    }

    /// Structural checks that do not need the signing key.
    pub fn validate_structure(&self) -> std::result::Result<(), String> {
        validate_decision_spec(
            &self.collection,
            self.kind,
            &self.subject,
            &self.category,
            &self.scope,
            self.target.as_deref(),
            self.expires_at.as_deref(),
            self.appeal.as_deref(),
            &self.issuer,
            &self.created_at,
        )
    }

    /// Whether a decision applies at `now_epoch`. Expiries and reversals are
    /// records, not timed decisions, so they are never "active" themselves.
    pub fn is_active_at(&self, now_epoch: i64) -> bool {
        if self.kind != DecisionKind::Decision {
            return false;
        }
        let Some(created) = epoch_seconds(&self.created_at) else {
            return false;
        };
        if created > now_epoch {
            return false;
        }
        self.expires_at
            .as_deref()
            .and_then(epoch_seconds)
            .is_none_or(|expires| now_epoch < expires)
    }

    /// Verify the signature over the canonical bytes with the key the record
    /// carries. The caller must still trust that key for `issuer`.
    pub fn verify(&self) -> Result<()> {
        self.validate_structure()
            .map_err(AtmosphericError::Message)?;
        let key = signing::verifying_key_from_spki_b64(&self.signing_key)?;
        signing::verify_b64(&key, &self.canonical_bytes(), &self.signature)
    }
}

/// Structural validation shared by record verification and input handling.
#[allow(clippy::too_many_arguments)]
pub fn validate_decision_spec(
    collection: &str,
    kind: DecisionKind,
    subject: &str,
    category: &str,
    scope: &str,
    target: Option<&str>,
    expires_at: Option<&str>,
    appeal: Option<&str>,
    issuer: &str,
    created_at: &str,
) -> std::result::Result<(), String> {
    if collection != DECISION_COLLECTION {
        return Err(format!("decision collection must be {DECISION_COLLECTION}"));
    }
    if !(issuer.starts_with("did:plc:") || issuer.starts_with("did:web:")) {
        return Err("decision issuer must be a did:plc or did:web identifier".into());
    }
    if issuer.len() > 256 || issuer.chars().any(char::is_whitespace) {
        return Err("decision issuer is malformed".into());
    }
    if tree::from_hex(subject).is_none() {
        return Err("decision subject must be a 64-character lower-hex identifier".into());
    }
    if category.is_empty() || category.len() > MAX_CATEGORY_LEN || has_control(category) {
        return Err(format!(
            "decision category must be 1..{MAX_CATEGORY_LEN} characters without control characters"
        ));
    }
    if scope.is_empty() || scope.len() > MAX_SCOPE_LEN || has_control(scope) {
        return Err(format!(
            "decision scope must be 1..{MAX_SCOPE_LEN} characters without control characters"
        ));
    }
    match (kind, target) {
        (DecisionKind::Decision, None) => {}
        (DecisionKind::Decision, Some(_)) => {
            return Err("a decision cannot target another decision".into());
        }
        (DecisionKind::Reversal | DecisionKind::Expiry, None) => {
            return Err("a reversal or expiry must target a decision rkey".into());
        }
        (DecisionKind::Reversal | DecisionKind::Expiry, Some(value)) => {
            if value.is_empty()
                || value.len() > MAX_TARGET_LEN
                || value.chars().any(char::is_whitespace)
                || has_control(value)
            {
                return Err("decision target is malformed".into());
            }
        }
    }
    let created = epoch_seconds(created_at)
        .ok_or_else(|| "decision createdAt must be RFC 3339 UTC".to_owned())?;
    if let Some(expires) = expires_at {
        let expires = epoch_seconds(expires)
            .ok_or_else(|| "decision expiresAt must be RFC 3339 UTC".to_owned())?;
        if expires <= created {
            return Err("decision expiresAt must be after createdAt".into());
        }
    }
    if let Some(appeal) = appeal
        && (appeal.len() > MAX_APPEAL_LEN || has_control(appeal))
    {
        return Err(format!(
            "decision appeal must be at most {MAX_APPEAL_LEN} characters without control characters"
        ));
    }
    Ok(())
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// A ledger record together with the repository key it was filed under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub rkey: String,
    pub record: DecisionRecord,
}

/// The current resolution of a decision after reversals and expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionState {
    Active,
    Expired,
    Reversed,
}

impl DecisionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Reversed => "reversed",
        }
    }
}

impl std::str::FromStr for DecisionState {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim() {
            "active" => Ok(Self::Active),
            "expired" => Ok(Self::Expired),
            "reversed" => Ok(Self::Reversed),
            other => Err(format!("unknown decision state: {other:?}")),
        }
    }
}

/// A decision plus its resolved state and, when ended by a record, the rkey
/// of that reversal or expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDecision {
    pub rkey: String,
    pub record: DecisionRecord,
    pub state: DecisionState,
    pub ended_by: Option<String>,
}

/// Resolve a collection walk into current state. Entries may arrive in any
/// order: the newest rkey wins when more than one record ends the same
/// decision. Signature verification and issuer trust are the caller's job.
pub fn resolve(entries: &[DecisionEntry], now_epoch: i64) -> Vec<ResolvedDecision> {
    let mut endings: BTreeMap<&str, (DecisionKind, &str)> = BTreeMap::new();
    for entry in entries {
        if matches!(
            entry.record.kind,
            DecisionKind::Reversal | DecisionKind::Expiry
        ) && let Some(target) = entry.record.target.as_deref()
        {
            let newer = endings
                .get(target)
                .is_none_or(|(_, rkey)| entry.rkey.as_str() > *rkey);
            if newer {
                endings.insert(target, (entry.record.kind, &entry.rkey));
            }
        }
    }
    let mut resolved: Vec<_> = entries
        .iter()
        .filter(|entry| entry.record.kind == DecisionKind::Decision)
        .map(|entry| {
            let (state, ended_by) = match endings.get(entry.rkey.as_str()) {
                Some((DecisionKind::Reversal, rkey)) => {
                    (DecisionState::Reversed, Some((*rkey).to_owned()))
                }
                Some((DecisionKind::Expiry, rkey)) => {
                    (DecisionState::Expired, Some((*rkey).to_owned()))
                }
                _ => {
                    let expired = entry
                        .record
                        .expires_at
                        .as_deref()
                        .and_then(epoch_seconds)
                        .is_some_and(|expires| expires <= now_epoch);
                    (
                        if expired {
                            DecisionState::Expired
                        } else {
                            DecisionState::Active
                        },
                        None,
                    )
                }
            };
            ResolvedDecision {
                rkey: entry.rkey.clone(),
                record: entry.record.clone(),
                state,
                ended_by,
            }
        })
        .collect();
    // rkeys are TIDs, so descending key order is newest-first.
    resolved.sort_by(|left, right| right.rkey.cmp(&left.rkey));
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::subject::identifier;

    fn record(kind: DecisionKind, target: Option<&str>) -> DecisionRecord {
        DecisionRecord {
            collection: DECISION_COLLECTION.into(),
            kind,
            subject: identifier(&[3u8; 32], "did:plc:subject"),
            category: "spam".into(),
            scope: "server".into(),
            target: target.map(str::to_owned),
            expires_at: None,
            appeal: None,
            issuer: "did:plc:server".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            signing_key: String::new(),
            signature: String::new(),
        }
    }

    #[test]
    fn signed_record_verifies_and_rejects_tampering() {
        let key = signing::generate();
        let mut decision = record(DecisionKind::Decision, None);
        decision.sign(&key);
        decision.verify().unwrap();

        decision.category = "harassment".into();
        assert!(decision.verify().is_err());
    }

    #[test]
    fn collection_is_part_of_the_signed_payload() {
        let key = signing::generate();
        let mut decision = record(DecisionKind::Decision, None);
        decision.sign(&key);
        decision.collection = "club.atmospheric.other.decision".into();
        assert!(decision.verify().is_err());
    }

    #[test]
    fn structure_rejects_plaintext_and_malformed_fields() {
        let mut decision = record(DecisionKind::Decision, None);
        decision.subject = "did:plc:subject".into();
        assert!(decision.validate_structure().is_err());
        decision.subject = identifier(&[3u8; 32], "did:plc:subject");

        decision.category = String::new();
        assert!(decision.validate_structure().is_err());
        decision.category = "spam".into();

        decision.expires_at = Some("2025-12-31T00:00:00Z".into());
        assert!(decision.validate_structure().is_err());
        decision.expires_at = None;

        let mut reversal = record(DecisionKind::Reversal, None);
        assert!(reversal.validate_structure().is_err());
        reversal.target = Some("3looooooooo".into());
        assert!(reversal.validate_structure().is_ok());
    }

    #[test]
    fn active_decisions_can_expire_by_time() {
        let mut decision = record(DecisionKind::Decision, None);
        assert!(decision.is_active_at(1_767_225_600)); // 2026-01-01T00:00:00Z
        decision.expires_at = Some("2026-02-01T00:00:00Z".into());
        assert!(decision.is_active_at(1_767_225_600));
        assert!(!decision.is_active_at(1_769_904_000)); // 2026-02-01T00:00:00Z
    }

    #[test]
    fn resolution_applies_reversal_expiry_and_time() {
        let key = signing::generate();
        let mut kept = record(DecisionKind::Decision, None);
        kept.sign(&key);
        let mut reversed = record(DecisionKind::Decision, None);
        reversed.sign(&key);
        let mut expired_by_record = record(DecisionKind::Decision, None);
        expired_by_record.sign(&key);

        let mut reversal = record(DecisionKind::Reversal, Some("2reversed1"));
        reversal.sign(&key);
        let mut expiry = record(DecisionKind::Expiry, Some("3expired001"));
        expiry.sign(&key);
        // Out of order: resolution must not depend on arrival order.
        let entries = vec![
            DecisionEntry {
                rkey: "2reversed1".into(),
                record: reversed.clone(),
            },
            DecisionEntry {
                rkey: "3expired001".into(),
                record: expired_by_record.clone(),
            },
            DecisionEntry {
                rkey: "1kept00001".into(),
                record: kept.clone(),
            },
            DecisionEntry {
                rkey: "4reversal1".into(),
                record: reversal.clone(),
            },
            DecisionEntry {
                rkey: "5expiry001".into(),
                record: expiry.clone(),
            },
        ];
        let resolved = resolve(&entries, 1_767_225_600);
        assert_eq!(resolved.len(), 3);
        let by_rkey: BTreeMap<_, _> = resolved
            .iter()
            .map(|item| (item.rkey.as_str(), item))
            .collect();
        assert_eq!(by_rkey["1kept00001"].state, DecisionState::Active);
        assert_eq!(by_rkey["2reversed1"].state, DecisionState::Reversed);
        assert_eq!(
            by_rkey["2reversed1"].ended_by.as_deref(),
            Some("4reversal1")
        );
        assert_eq!(by_rkey["3expired001"].state, DecisionState::Expired);
        assert_eq!(
            by_rkey["3expired001"].ended_by.as_deref(),
            Some("5expiry001")
        );
    }
}
