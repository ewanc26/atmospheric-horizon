//! Reading a published Counting Stones tally (#193): resolve the account,
//! page the result collection from its own PDS unauthenticated, pick the
//! newest structurally valid record, and verify its signature. The record
//! carries its own public key, so verification is self-contained — any player
//! can check a tally without trusting the server that published it.

use crate::error::{AtmosphericError, Result};
use crate::poll::record::{POLL_COLLECTION, PollRecord};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// The verified view of a published tally for the HUD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollDigest {
    pub did: String,
    pub rkey: String,
    pub at_uri: String,
    /// The parsed record, including its signature and public key.
    pub record: PollRecord,
    /// True when the signature verifies over the canonical bytes and the
    /// structure passes (unique participants, tally equals participant count).
    pub verified: bool,
}

/// Pick the newest structurally valid record (by `createdAt`, with the rkey
/// as a deterministic tie-break). Returns `None` when the repo has no
/// parseable poll result.
pub fn latest(records: &[(String, serde_json::Value)]) -> Option<(String, serde_json::Value)> {
    let mut best: Option<(String, String, serde_json::Value)> = None;
    for (rkey, value) in records {
        let valid = serde_json::from_value::<PollRecord>(value.clone())
            .map(|record| record.validate_structure().is_ok())
            .unwrap_or(false);
        if !valid {
            continue;
        }
        let created = value["createdAt"].as_str().unwrap_or("").to_string();
        let better = best.as_ref().is_none_or(|(best_rkey, best_created, _)| {
            (&created, rkey) > (best_created, best_rkey)
        });
        if better {
            best = Some((rkey.clone(), created, value.clone()));
        }
    }
    best.map(|(rkey, _, value)| (rkey, value))
}

/// Build a digest of a published record without any network. `verified` is
/// the signature verdict; structural validity is enforced before this runs.
pub fn digest(did: &str, rkey: &str, value: &serde_json::Value) -> Result<PollDigest> {
    let record = serde_json::from_value::<PollRecord>(value.clone()).map_err(|error| {
        AtmosphericError::Message(format!(
            "the poll record does not match its schema: {error}"
        ))
    })?;
    let verified = record.verify().is_ok();
    Ok(PollDigest {
        did: did.to_string(),
        rkey: rkey.to_string(),
        at_uri: format!("at://{did}/{POLL_COLLECTION}/{rkey}"),
        record,
        verified,
    })
}

/// Read an account's latest published poll result: resolve the subject (a DID
/// or handle), page the collection from its own PDS unauthenticated, pick the
/// newest valid record and build the digest. Works logged out.
pub async fn fetch_poll(
    http: &Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<(String, String, Option<PollDigest>)> {
    let (did, pds) = crate::social::servers::resolve_server(http, resolver, raw).await?;
    let records = crate::social::servers::list_records(http, &pds, &did, POLL_COLLECTION).await?;
    let Some((rkey, value)) = latest(&records) else {
        return Ok((did, pds, None));
    };
    let poll = digest(&did, &rkey, &value)?;
    Ok((did, pds, Some(poll)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> PollRecord {
        let mut record = PollRecord {
            question: "Where next?".into(),
            options: vec!["Nether".into(), "End".into()],
            closes_at: "2026-11-26T00:00:00Z".into(),
            created_at: "2026-11-25T00:00:00Z".into(),
            root: "a".repeat(64),
            participants: vec!["did:plc:a".into(), "did:plc:b".into()],
            tally: vec![1, 1],
            signing_key: String::new(),
            signature: String::new(),
        };
        record.sign(&crate::signing::generate());
        record
    }

    #[test]
    fn latest_picks_the_newest_valid_record() {
        let newer = record();
        let mut older = record();
        older.created_at = "2026-11-24T00:00:00Z".into();
        older.sign(&crate::signing::generate());
        let mut broken = record();
        broken.created_at = "2026-11-27T00:00:00Z".into();
        broken.root = "bad".into();
        let records = vec![
            ("old".into(), serde_json::to_value(&older).unwrap()),
            ("new".into(), serde_json::to_value(&newer).unwrap()),
            ("broken".into(), serde_json::to_value(&broken).unwrap()),
        ];
        let (rkey, _) = latest(&records).unwrap();
        assert_eq!(rkey, "new");
        assert!(latest(&[]).is_none());
    }

    #[test]
    fn digest_reports_the_signature_verdict() {
        let signed = record();
        let value = serde_json::to_value(&signed).unwrap();
        let result = digest("did:plc:a", "rkey", &value).unwrap();
        assert!(result.verified);
        assert_eq!(
            result.at_uri,
            format!("at://did:plc:a/{POLL_COLLECTION}/rkey")
        );

        let mut tampered = signed;
        tampered.tally = vec![2, 0];
        let value = serde_json::to_value(&tampered).unwrap();
        assert!(!digest("did:plc:a", "rkey", &value).unwrap().verified);
    }

    #[test]
    fn digest_rejects_a_wrong_schema() {
        assert!(digest("did:plc:a", "rkey", &serde_json::json!({"nope": true})).is_err());
    }
}
