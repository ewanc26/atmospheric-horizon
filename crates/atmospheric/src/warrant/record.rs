//! The signed warrant credential (#192). A warrant lives in the *player's*
//! repo but is signed by an issuer: the player may self-publish it, yet it is
//! worthless unless the signature comes from a DID/key/collection the worker
//! pins in its allowlist. That is what makes a cosmetic rank harder to forge
//! than a plain profile claim, without giving the issuer write access to a
//! player's repo.
//!
//! Signatures cover a canonical JSON encoding of every field except the
//! signature itself, using the same Ed25519 SPKI helpers as the poll tallies
//! and atpcraft endorsements. A record carries its issuer DID and public key
//! so a reader can verify it in one pass; the allowlist is the trust root.

use crate::card::digest::epoch_seconds;
use crate::error::{AtmosphericError, Result};
use crate::signing::{SigningKey, public_key_spki_b64, sign_b64, verify_b64};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

/// The default collection for community badges. Operators may pin other
/// collections; the collection is part of the signed payload so a warrant
/// cannot be replayed under a different allowlisted lexicon.
pub const DEFAULT_WARRANT_COLLECTION: &str = "club.atmospheric.warrant";

/// Longest accepted cosmetic title, in bytes.
pub const MAX_TITLE_BYTES: usize = 120;
/// Longest accepted collection NSID, in bytes.
pub const MAX_COLLECTION_BYTES: usize = 256;

/// Validate the caller-supplied fields of a warrant before it is signed or
/// accepted. The same checks run on every read, so a malformed record never
/// reaches the allowlist comparison or produces a name-plate line.
pub fn validate_warrant_spec(
    collection: &str,
    issuer: &str,
    subject: &str,
    title: &str,
    issued_at: &str,
    expires_at: Option<&str>,
) -> std::result::Result<(), String> {
    if collection.trim().is_empty() || collection.len() > MAX_COLLECTION_BYTES {
        return Err("collection is empty or too long".into());
    }
    if !collection
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err("collection is not a valid NSID".into());
    }
    if !valid_did(issuer) {
        return Err("issuer is not a valid DID".into());
    }
    if !valid_did(subject) {
        return Err("subject is not a valid DID".into());
    }
    if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err("title is empty or too long".into());
    }
    if title.chars().any(|c| c.is_control()) {
        return Err("title contains a control character".into());
    }
    let issued = epoch_seconds(issued_at).ok_or("issuedAt is not an RFC 3339 UTC timestamp")?;
    if let Some(expires) = expires_at {
        let expires = epoch_seconds(expires).ok_or("expiresAt is not an RFC 3339 UTC timestamp")?;
        if expires <= issued {
            return Err("expiresAt must be after issuedAt".into());
        }
    }
    Ok(())
}

/// A DID is `did:<method>:<method-specific-id>` with no whitespace or control
/// characters. This is deliberately a shape check, not a resolution: the
/// allowlist supplies the trusted issuer and the repo supplies the subject.
fn valid_did(did: &str) -> bool {
    let mut parts = did.splitn(3, ':');
    matches!(parts.next(), Some("did"))
        && parts.next().is_some_and(|method| {
            !method.is_empty() && method.bytes().all(|b| b.is_ascii_lowercase())
        })
        && parts
            .next()
            .is_some_and(|id| !id.is_empty() && !id.chars().any(char::is_whitespace))
        && !did.chars().any(char::is_control)
}

/// A signed warrant credential as stored in a player's repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarrantRecord {
    /// The collection the record is filed under; signed to stop replay under
    /// a different allowlisted lexicon.
    pub collection: String,
    /// DID of the issuer whose pinned key must verify the signature.
    pub issuer: String,
    /// DID of the player this cosmetic rank belongs to.
    pub subject: String,
    /// The cosmetic name-plate line, e.g. "Community builder".
    pub title: String,
    /// When the issuer granted the warrant, RFC 3339 UTC.
    pub issued_at: String,
    /// Optional expiry, RFC 3339 UTC. Absent means the warrant does not time
    /// out; deleting the record is the other revocation path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The issuer's Ed25519 public key, base64 of its SPKI encoding.
    pub signing_key: String,
    /// Base64 (no padding) Ed25519 signature over the canonical bytes.
    pub signature: String,
}

/// The signed subset, in a fixed field order shared by signer and verifier.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    collection: &'a str,
    issuer: &'a str,
    subject: &'a str,
    title: &'a str,
    issued_at: &'a str,
    expires_at: Option<&'a str>,
    signing_key: &'a str,
}

impl WarrantRecord {
    /// The exact bytes the signature covers: every field except `signature`.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let payload = Payload {
            collection: &self.collection,
            issuer: &self.issuer,
            subject: &self.subject,
            title: &self.title,
            issued_at: &self.issued_at,
            expires_at: self.expires_at.as_deref(),
            signing_key: &self.signing_key,
        };
        // No maps, floats or custom serialisers: serialisation cannot fail.
        serde_json::to_vec(&payload).expect("the warrant payload is always serialisable")
    }

    /// Fill in the issuer's public key and sign. Call after every other field
    /// is set; re-signing replaces any previous signature.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signing_key = public_key_spki_b64(&key.verifying_key());
        self.signature = sign_b64(key, &self.canonical_bytes());
    }

    /// Structural checks that do not need the signing key.
    pub fn validate_structure(&self) -> std::result::Result<(), String> {
        validate_warrant_spec(
            &self.collection,
            &self.issuer,
            &self.subject,
            &self.title,
            &self.issued_at,
            self.expires_at.as_deref(),
        )
    }

    /// Whether the warrant is still valid at `now_epoch`. An absent
    /// `expiresAt` never times out; a warrant issued in the future is not yet
    /// active, so a backdated or premature grant cannot flash a rank early.
    pub fn is_active_at(&self, now_epoch: i64) -> bool {
        let Some(issued) = epoch_seconds(&self.issued_at) else {
            return false;
        };
        if issued > now_epoch {
            return false;
        }
        match &self.expires_at {
            None => true,
            Some(expires) => epoch_seconds(expires).is_some_and(|expires| now_epoch < expires),
        }
    }

    /// Verify the signature over the canonical bytes with the key the record
    /// carries. The caller must still compare that key to the pinned allowlist
    /// entry — a self-signed warrant is otherwise perfectly valid.
    pub fn verify(&self) -> Result<VerifyingKey> {
        self.validate_structure()
            .map_err(AtmosphericError::Message)?;
        let key = crate::signing::verifying_key_from_spki_b64(&self.signing_key)?;
        verify_b64(&key, &self.canonical_bytes(), &self.signature)?;
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> WarrantRecord {
        let mut record = WarrantRecord {
            collection: DEFAULT_WARRANT_COLLECTION.into(),
            issuer: "did:plc:issuer".into(),
            subject: "did:plc:player".into(),
            title: "Community builder".into(),
            issued_at: "2026-01-01T00:00:00Z".into(),
            expires_at: None,
            signing_key: String::new(),
            signature: String::new(),
        };
        record.sign(&crate::signing::generate());
        record
    }

    #[test]
    fn a_signed_warrant_verifies_and_round_trips() {
        let signed = record();
        signed.verify().unwrap();
        let json = serde_json::to_string(&signed).unwrap();
        assert!(json.contains(r#""issuedAt""#));
        let back: WarrantRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, signed);
        back.verify().unwrap();
    }

    #[test]
    fn canonical_bytes_exclude_the_signature() {
        let signed = record();
        let mut unsigned = signed.clone();
        unsigned.signature = "not the signature".into();
        assert_eq!(unsigned.canonical_bytes(), signed.canonical_bytes());
    }

    #[test]
    fn tampering_with_any_signed_field_fails() {
        let signed = record();
        for mutated in [
            WarrantRecord {
                title: "Staff".into(),
                ..signed.clone()
            },
            WarrantRecord {
                subject: "did:plc:someone-else".into(),
                ..signed.clone()
            },
            WarrantRecord {
                collection: "club.atmospheric.other".into(),
                ..signed.clone()
            },
            WarrantRecord {
                expires_at: Some("2027-01-01T00:00:00Z".into()),
                ..signed.clone()
            },
        ] {
            assert!(mutated.verify().is_err());
        }
    }

    #[test]
    fn a_warrant_is_only_active_within_its_time_window() {
        let mut signed = record();
        assert!(signed.is_active_at(1_800_000_000));
        // Issued in the future: not active yet.
        signed.issued_at = "2030-01-01T00:00:00Z".into();
        assert!(!signed.is_active_at(1_800_000_000));
        // Expired.
        signed.issued_at = "2026-01-01T00:00:00Z".into();
        signed.expires_at = Some("2026-06-01T00:00:00Z".into());
        assert!(!signed.is_active_at(1_800_000_000));
        assert!(signed.is_active_at(1_770_000_000));
    }

    #[test]
    fn malformed_specs_are_rejected_before_signing() {
        assert!(
            validate_warrant_spec(
                "club.atmospheric.warrant",
                "did:plc:issuer",
                "did:plc:player",
                "Rank",
                "2026-01-01T00:00:00Z",
                None
            )
            .is_ok()
        );
        assert!(
            validate_warrant_spec(
                "",
                "did:plc:i",
                "did:plc:p",
                "Rank",
                "2026-01-01T00:00:00Z",
                None
            )
            .is_err()
        );
        assert!(
            validate_warrant_spec(
                "club.atmospheric.warrant",
                "issuer",
                "did:plc:p",
                "Rank",
                "2026-01-01T00:00:00Z",
                None
            )
            .is_err()
        );
        assert!(
            validate_warrant_spec(
                "club.atmospheric.warrant",
                "did:plc:i",
                "did:plc:p",
                "",
                "2026-01-01T00:00:00Z",
                None
            )
            .is_err()
        );
        assert!(
            validate_warrant_spec(
                "club.atmospheric.warrant",
                "did:plc:i",
                "did:plc:p",
                "Rank",
                "2026-01-01T00:00:00Z",
                Some("2025-01-01T00:00:00Z")
            )
            .is_err()
        );
    }
}
