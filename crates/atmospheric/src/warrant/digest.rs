//! Reading warrants (#192): list the allowlisted collections from the
//! subject's own repo, keep only records whose signature verifies against the
//! exact pinned issuer key, whose subject matches, and that are still active.
//! There is no push and no ambient action: a deleted record is simply absent
//! on the next fetch, which is the revocation path.
//!
//! Warrants are cosmetic-only. Nothing here gates entry or grants a gameplay
//! capability; the digest is a list of name-plate lines for the HUD.

use crate::config::WarrantIssuer;
use crate::error::Result;
use crate::signing::verifying_key_from_spki_b64;
use crate::warrant::record::WarrantRecord;
use ed25519_dalek::VerifyingKey;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// One verified cosmetic line for the HUD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarrantLine {
    /// The cosmetic rank text to show on the name plate.
    pub title: String,
    /// The pinned issuer DID that granted it.
    pub issuer: String,
    /// The collection it was filed under.
    pub collection: String,
}

/// The verified, cosmetic-only view of a player's active warrants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarrantDigest {
    /// The subject's resolved DID.
    pub did: String,
    /// Active, verified name-plate lines in a deterministic order.
    pub warrants: Vec<WarrantLine>,
}

/// Verify one parsed record against the allowlist. Returns the granted line
/// when the issuer DID and collection are pinned, the signature verifies with
/// the pinned key (not merely a self-declared one), the subject matches the
/// player whose repo we read, and the warrant is active at `now_epoch`.
pub fn verify_against(
    record: &WarrantRecord,
    issuers: &[WarrantIssuer],
    subject: &str,
    now_epoch: i64,
) -> Option<WarrantLine> {
    let issuer = issuers
        .iter()
        .find(|entry| entry.collection == record.collection && entry.did == record.issuer)?;
    // A self-signed warrant is worthless: the record's key must equal the
    // pinned key byte-for-byte, not just verify the record it declares.
    let record_key: VerifyingKey = record.verify().ok()?;
    let pinned_key = verifying_key_from_spki_b64(&issuer.public_key).ok()?;
    if record_key.to_bytes() != pinned_key.to_bytes() {
        return None;
    }
    if record.subject != subject || !record.is_active_at(now_epoch) {
        return None;
    }
    Some(WarrantLine {
        title: record.title.clone(),
        issuer: record.issuer.clone(),
        collection: record.collection.clone(),
    })
}

/// Build a digest from already-listed `(collection, value)` records without a
/// network. Lines are de-duplicated and sorted so the HUD output is stable
/// regardless of repo page order.
pub fn digest(
    did: &str,
    records: &[(String, serde_json::Value)],
    issuers: &[WarrantIssuer],
    now_epoch: i64,
) -> WarrantDigest {
    let mut lines: Vec<WarrantLine> = records
        .iter()
        .filter_map(|(collection, value)| {
            let record = serde_json::from_value::<WarrantRecord>(value.clone()).ok()?;
            // The collection from the listing must match the signed one; a
            // value filed under the wrong collection is not accepted.
            if record.collection != *collection {
                return None;
            }
            verify_against(&record, issuers, did, now_epoch)
        })
        .collect();
    lines.sort_by(|a, b| {
        (&a.title, &a.issuer, &a.collection).cmp(&(&b.title, &b.issuer, &b.collection))
    });
    lines.dedup();
    WarrantDigest {
        did: did.to_string(),
        warrants: lines,
    }
}

/// Current Unix time, used for the expiry check. A clock failure yields zero
/// (the epoch), which makes every dated warrant look expired — fail closed.
fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// Read a player's active cosmetic warrants: resolve the subject, list each
/// pinned collection from their own PDS unauthenticated, and verify. Works
/// logged out. An empty allowlist returns an empty digest without a fetch.
pub async fn fetch_warrants(
    http: &Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
    issuers: &[WarrantIssuer],
) -> Result<(String, String, WarrantDigest)> {
    let (did, pds) = crate::social::servers::resolve_server(http, resolver, raw).await?;
    if issuers.is_empty() {
        return Ok((
            did.clone(),
            pds,
            WarrantDigest {
                did,
                warrants: Vec::new(),
            },
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut records = Vec::new();
    for issuer in issuers {
        if !seen.insert(issuer.collection.as_str()) {
            continue;
        }
        let listed =
            crate::social::servers::list_records(http, &pds, &did, &issuer.collection).await?;
        for (rkey, value) in listed {
            records.push((issuer.collection.clone(), value));
            let _ = rkey; // the digest is content-addressed, not rkey-addressed
        }
    }
    Ok((
        did.clone(),
        pds,
        digest(&did, &records, issuers, now_epoch()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{generate, public_key_spki_b64};
    use crate::warrant::record::DEFAULT_WARRANT_COLLECTION;

    fn issuer(did: &str, key: &str) -> WarrantIssuer {
        WarrantIssuer {
            did: did.into(),
            collection: DEFAULT_WARRANT_COLLECTION.into(),
            public_key: key.into(),
        }
    }

    #[test]
    fn a_pinned_and_signed_warrant_produces_a_line() {
        let key = generate();
        let pin = public_key_spki_b64(&key.verifying_key());
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
        record.sign(&key);
        let value = serde_json::to_value(&record).unwrap();
        let digest = digest(
            "did:plc:player",
            &[(DEFAULT_WARRANT_COLLECTION.into(), value)],
            &[issuer("did:plc:issuer", &pin)],
            1_800_000_000,
        );
        assert_eq!(digest.warrants.len(), 1);
        assert_eq!(digest.warrants[0].title, "Community builder");
    }

    #[test]
    fn a_self_signed_or_unpinned_warrant_is_ignored() {
        let key = generate();
        let mut record = WarrantRecord {
            collection: DEFAULT_WARRANT_COLLECTION.into(),
            issuer: "did:plc:issuer".into(),
            subject: "did:plc:player".into(),
            title: "Fake staff".into(),
            issued_at: "2026-01-01T00:00:00Z".into(),
            expires_at: None,
            signing_key: String::new(),
            signature: String::new(),
        };
        record.sign(&key); // self-signed: the key is not pinned anywhere
        let value = serde_json::to_value(&record).unwrap();
        let records = [(DEFAULT_WARRANT_COLLECTION.into(), value)];

        // Pinned to a different issuer DID.
        let wrong_did = digest(
            "did:plc:player",
            &records,
            &[issuer(
                "did:plc:someone-else",
                &public_key_spki_b64(&key.verifying_key()),
            )],
            1_800_000_000,
        );
        assert!(wrong_did.warrants.is_empty());

        // Same DID but pinned to a different key: still ignored.
        let wrong_key = generate();
        let wrong_key = digest(
            "did:plc:player",
            &records,
            &[issuer(
                "did:plc:issuer",
                &public_key_spki_b64(&wrong_key.verifying_key()),
            )],
            1_800_000_000,
        );
        assert!(wrong_key.warrants.is_empty());

        // Pinned correctly but for a different subject.
        let not_mine = digest(
            "did:plc:someone-else",
            &records,
            &[issuer(
                "did:plc:issuer",
                &public_key_spki_b64(&key.verifying_key()),
            )],
            1_800_000_000,
        );
        assert!(not_mine.warrants.is_empty());
    }

    #[test]
    fn a_tampered_or_wrong_collection_record_is_ignored() {
        let key = generate();
        let pin = public_key_spki_b64(&key.verifying_key());
        let mut record = WarrantRecord {
            collection: DEFAULT_WARRANT_COLLECTION.into(),
            issuer: "did:plc:issuer".into(),
            subject: "did:plc:player".into(),
            title: "Rank".into(),
            issued_at: "2026-01-01T00:00:00Z".into(),
            expires_at: None,
            signing_key: String::new(),
            signature: String::new(),
        };
        record.sign(&key);
        let mut value = serde_json::to_value(&record).unwrap();
        value["title"] = serde_json::json!("Tampered");
        let tampered = digest(
            "did:plc:player",
            &[(DEFAULT_WARRANT_COLLECTION.into(), value)],
            &[issuer("did:plc:issuer", &pin)],
            1_800_000_000,
        );
        assert!(tampered.warrants.is_empty());

        // A valid record listed under a different collection is not accepted.
        let value = serde_json::to_value(&record).unwrap();
        let wrong_collection = digest(
            "did:plc:player",
            &[("club.atmospheric.other".into(), value)],
            &[issuer("did:plc:issuer", &pin)],
            1_800_000_000,
        );
        assert!(wrong_collection.warrants.is_empty());
    }

    #[test]
    fn lines_are_sorted_and_deduplicated() {
        let key = generate();
        let pin = public_key_spki_b64(&key.verifying_key());
        let entry = issuer("did:plc:issuer", &pin);
        let mut records = Vec::new();
        for title in ["Zeta", "Alpha", "Alpha"] {
            let mut record = WarrantRecord {
                collection: DEFAULT_WARRANT_COLLECTION.into(),
                issuer: "did:plc:issuer".into(),
                subject: "did:plc:player".into(),
                title: title.into(),
                issued_at: "2026-01-01T00:00:00Z".into(),
                expires_at: None,
                signing_key: String::new(),
                signature: String::new(),
            };
            record.sign(&key);
            records.push((
                DEFAULT_WARRANT_COLLECTION.into(),
                serde_json::to_value(&record).unwrap(),
            ));
        }
        let digest = digest("did:plc:player", &records, &[entry], 1_800_000_000);
        assert_eq!(
            digest
                .warrants
                .iter()
                .map(|line| line.title.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Zeta"]
        );
    }
}
