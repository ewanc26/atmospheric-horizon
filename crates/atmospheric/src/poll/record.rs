//! The published Counting Stones tally record (#193): a server-signed Merkle
//! root plus the participant list and aggregate tally, written as one record.
//! The record is adjudication infrastructure, never user-authored content.
//!
//! Signatures cover a canonical JSON encoding of every field except the
//! signature itself, using the same Ed25519 key helpers as the atpcraft
//! endorsements. A reader with only the public key can verify the root and
//! tally were not altered; a voter with a private receipt can additionally
//! prove their ballot is in the tree (see [`super::ballot`]).

use crate::card::digest::epoch_seconds;
use crate::error::{AtmosphericError, Result};
use crate::signing::{SigningKey, public_key_spki_b64, sign_b64, verify_b64};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

/// Collection for the published tally record. The authority domain matches
/// the project's other NSIDs; changing it changes the published collection.
pub const POLL_COLLECTION: &str = "click.croft.atmospheric.poll.result";

/// Longest accepted question, in bytes.
pub const MAX_QUESTION_BYTES: usize = 280;
/// Longest accepted choice label, in bytes.
pub const MAX_OPTION_BYTES: usize = 120;
/// Fewest choices a poll may offer.
pub const MIN_OPTIONS: usize = 2;
/// Most choices a poll may offer. Both counts fit in the tally's `u16` choice.
pub const MAX_OPTIONS: usize = 64;

/// Validate the caller-supplied parts of a poll before a ballot box opens.
/// The same checks run again on every published record and on every read, so
/// a malformed poll can never reach the signing key or the PDS.
pub fn validate_poll_spec(
    question: &str,
    options: &[String],
    closes_at: &str,
) -> std::result::Result<(), String> {
    if question.trim().is_empty() || question.len() > MAX_QUESTION_BYTES {
        return Err("question is empty or too long".into());
    }
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
        return Err(format!(
            "a poll needs between {MIN_OPTIONS} and {MAX_OPTIONS} options"
        ));
    }
    for (index, option) in options.iter().enumerate() {
        if option.trim().is_empty() || option.len() > MAX_OPTION_BYTES {
            return Err(format!("option {} is empty or too long", index + 1));
        }
    }
    if epoch_seconds(closes_at).is_none() {
        return Err("closes_at is not an RFC 3339 UTC timestamp".into());
    }
    Ok(())
}

/// The signed tally record as published in `POLL_COLLECTION`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollRecord {
    /// The question put to voters.
    pub question: String,
    /// Choice labels, indexed from zero. A vote stores the index, not the text.
    pub options: Vec<String>,
    /// RFC 3339 UTC time the poll closes. Votes after this time are refused.
    pub closes_at: String,
    /// When the tally was published, RFC 3339 UTC.
    pub created_at: String,
    /// Lower-hex sha-256 of the Merkle root over all ballots.
    pub root: String,
    /// The DIDs the server claims voted, in submission order. One per voter.
    pub participants: Vec<String>,
    /// Votes per choice, in the same order as `options`.
    pub tally: Vec<u64>,
    /// The server's Ed25519 public key, base64 of its SPKI encoding.
    pub signing_key: String,
    /// Base64 (no padding) Ed25519 signature over the canonical bytes.
    pub signature: String,
}

/// The signed subset of a record, in a fixed field order. `serde_json`
/// serializes struct fields in declaration order, so this is a stable
/// canonical form shared by the signer and every verifier.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    question: &'a str,
    options: &'a [String],
    closes_at: &'a str,
    created_at: &'a str,
    root: &'a str,
    participants: &'a [String],
    tally: &'a [u64],
    signing_key: &'a str,
}

impl PollRecord {
    /// The exact bytes the signature covers: every field except `signature`.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let payload = Payload {
            question: &self.question,
            options: &self.options,
            closes_at: &self.closes_at,
            created_at: &self.created_at,
            root: &self.root,
            participants: &self.participants,
            tally: &self.tally,
            signing_key: &self.signing_key,
        };
        // Serialising this struct cannot fail (no maps, no floats, no custom
        // serialisers), so the expectation documents the invariant.
        serde_json::to_vec(&payload).expect("the poll payload is always serialisable")
    }

    /// Fill in the public key and sign the record. Call after the tally and
    /// root are set; re-signing replaces any previous signature.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signing_key = public_key_spki_b64(&key.verifying_key());
        self.signature = sign_b64(key, &self.canonical_bytes());
    }

    /// Structural checks that do not need the signing key. A record that
    /// fails these is not worth verifying cryptographically.
    pub fn validate_structure(&self) -> std::result::Result<(), String> {
        validate_poll_spec(&self.question, &self.options, &self.closes_at)?;
        if self.tally.len() != self.options.len() {
            return Err(format!(
                "options and tally must have the same length, between {MIN_OPTIONS} and {MAX_OPTIONS}"
            ));
        }
        if epoch_seconds(&self.created_at).is_none() {
            return Err("created_at is not an RFC 3339 UTC timestamp".into());
        }
        if super::tree::from_hex(&self.root).is_none() {
            return Err("root is not a 64-character lower-hex sha-256".into());
        }
        let votes: u64 = self.tally.iter().sum();
        if votes != self.participants.len() as u64 {
            return Err("tally does not add up to the participant count".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for did in &self.participants {
            if did.is_empty() {
                return Err("a participant has an empty DID".into());
            }
            if !seen.insert(did) {
                return Err("a DID appears more than once in participants".into());
            }
        }
        Ok(())
    }

    /// Verify the signature over the canonical bytes. Returns the verifying
    /// key so the caller can also check it against a trusted key if needed.
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

    fn record() -> PollRecord {
        let mut record = PollRecord {
            question: "Where should we go next?".into(),
            options: vec!["Nether".into(), "End".into()],
            closes_at: "2026-11-26T00:00:00Z".into(),
            created_at: "2026-11-25T00:00:00Z".into(),
            root: "a".repeat(64),
            participants: vec!["did:plc:alice".into(), "did:plc:bob".into()],
            tally: vec![1, 1],
            signing_key: String::new(),
            signature: String::new(),
        };
        record.sign(&crate::signing::generate());
        record
    }

    #[test]
    fn signed_record_verifies_and_round_trips() {
        let signed = record();
        signed.verify().unwrap();
        let json = serde_json::to_string(&signed).unwrap();
        assert!(json.contains(r#""closesAt""#));
        assert!(json.contains(r#""createdAt""#));
        assert!(json.contains(r#""signingKey""#));
        let back: PollRecord = serde_json::from_str(&json).unwrap();
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
        let mut tampered = signed.clone();
        tampered.tally = vec![2, 0];
        assert!(tampered.verify().is_err());

        let mut tampered = signed.clone();
        tampered.question = "Different question?".into();
        assert!(tampered.verify().is_err());

        let mut tampered = signed.clone();
        tampered.root = "b".repeat(64);
        assert!(tampered.verify().is_err());

        let mut tampered = signed.clone();
        tampered.signature = crate::signing::sign_b64(&crate::signing::generate(), b"other");
        assert!(tampered.verify().is_err());
    }

    #[test]
    fn structure_rejects_bad_shapes() {
        let signed = record();

        let mut bad = signed.clone();
        bad.tally = vec![1];
        assert!(bad.verify().unwrap_err().to_string().contains("tally"));

        let mut bad = signed.clone();
        bad.root = "ABC".into();
        assert!(bad.verify().unwrap_err().to_string().contains("root"));

        let mut bad = signed.clone();
        bad.closes_at = "yesterday".into();
        assert!(bad.verify().unwrap_err().to_string().contains("closes_at"));

        let mut bad = signed.clone();
        bad.participants = vec!["did:plc:alice".into(), "did:plc:alice".into()];
        assert!(
            bad.verify()
                .unwrap_err()
                .to_string()
                .contains("more than once")
        );

        let mut bad = signed.clone();
        bad.options = vec!["only one".into()];
        bad.tally = vec![2];
        assert!(
            bad.verify()
                .unwrap_err()
                .to_string()
                .contains("between 2 and 64 options")
        );
    }
}
