//! Reading other accounts' `at.atpcraft.server.profile` records — the read
//! side of atpcraft's server descriptor, completing M10, reimplemented from
//! the lexicon semantics only: no code is copied from atpcraft (its repo
//! carries no licence).
//!
//! A server profile lives in the account's own repo at rkey `self`, so the
//! read is: resolve the account (handle → DID via the same directory service
//! OAuth login uses, then the DID document's `#atproto_pds` endpoint), page
//! `com.atproto.repo.listRecords` for the server-profile collection
//! **unauthenticated** against that PDS, prefer the `self` record key and
//! otherwise take the newest structurally valid record, and then check its
//! `signingKey` block against the keys you trust — the mod's own signing key
//! plus `trusted_server_keys` from the settings file. A malformed record is
//! skipped rather than fatal.

use crate::error::{AtmosphericError, Result};
use crate::labels;
use crate::oauth::{pds_endpoint, resolve_did_document};
use crate::records;
use crate::signing;
use ed25519_dalek::VerifyingKey;
use reqwest::Client;
use std::collections::HashMap;

/// `com.atproto.repo.listRecords` page size.
pub const LIST_LIMIT: u32 = 100;

/// Defensive pagination cap (100 pages = 10 000 records).
pub const MAX_PAGES: usize = 100;

/// The trust verdict on a server profile's `signingKey` block. The profile
/// record carries no signature itself (repo authentication only), so the
/// verdict says whether its published key is one you trust — never that it
/// *signed* anything.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ServerVerdict {
    /// `key_id` is a key you trust (own key or a `trusted_server_keys`
    /// entry) and its published public key matches.
    Trusted { key_id: String },
    /// `key_id` is not in the trusted set.
    Unknown,
    /// `key_id` is trusted but its `publicKey` does not match the key you
    /// have for that id (the record was tampered with or is foreign).
    KeyMismatch { key_id: String },
    /// The record is not a structurally valid server profile.
    Malformed,
}

/// What the HUD shows for a server profile (M19).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerDigest {
    pub did: String,
    pub rkey: String,
    pub at_uri: String,
    pub name: String,
    pub description: String,
    pub address: String,
    pub minecraft_version: String,
    pub atpcraft_version: String,
    pub capabilities: Vec<String>,
    pub operator_handle: String,
    pub created_at: Option<String>,
    pub updated_at: String,
    pub signing_key_id: String,
    pub verdict: ServerVerdict,
}

/// Resolve a server reference (a DID or handle — not a record URI) to the
/// account's DID and the PDS endpoint its DID document advertises.
pub async fn resolve_server(
    http: &Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<(String, String)> {
    let reference = labels::parse_subject_reference(raw)?;
    let resolved = labels::resolve_subject(http, resolver, &reference).await?;
    if resolved.collection.is_some() {
        return Err(AtmosphericError::Message(
            "want a server account (a DID or a handle), not a record URI".into(),
        ));
    }
    let did = resolved.pattern;
    let doc = resolve_did_document(http, resolver, &did).await?;
    let pds = pds_endpoint(&doc)?;
    Ok((did, pds))
}

/// Page `com.atproto.repo.listRecords` for `collection` in `repo`'s own PDS,
/// unauthenticated (public repos serve this). Returns `(rkey, value)` pairs
/// in page order.
pub async fn list_records(
    http: &Client,
    pds: &str,
    repo: &str,
    collection: &str,
) -> Result<Vec<(String, serde_json::Value)>> {
    let base = pds.trim_end_matches('/');
    let mut out: Vec<(String, serde_json::Value)> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut request = http
            .get(format!("{base}/xrpc/com.atproto.repo.listRecords"))
            .query(&[
                ("repo", repo),
                ("collection", collection),
                ("limit", &LIST_LIMIT.to_string()),
            ]);
        if let Some(cursor) = &cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        let response = request.send().await.map_err(AtmosphericError::protocol)?;
        let status = response.status();
        let body = response.text().await.map_err(AtmosphericError::protocol)?;
        if !status.is_success() {
            return Err(AtmosphericError::Message(format!(
                "listRecords for {collection} in {repo} failed with HTTP {status}: {body}"
            )));
        }
        let page: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(records) = page["records"].as_array() {
            for record in records {
                let uri = record["uri"].as_str().unwrap_or_default();
                let rkey = uri.rsplit('/').next().unwrap_or_default().to_string();
                if let Some(value) = record.get("value") {
                    out.push((rkey, value.clone()));
                }
            }
        }
        pages += 1;
        match page["cursor"].as_str() {
            Some(next) if !next.is_empty() && pages < MAX_PAGES => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

/// Whether `value` is structurally a server profile record: the required
/// fields (`name`, `minecraft_version`, `atpcraft_version`, `signing_key`
/// with `key_id`/`public_key`/`alg`) are present. Invalid records are
/// skipped rather than fatal.
fn structural_valid(value: &serde_json::Value) -> bool {
    let signing_key = match value.get("signingKey") {
        Some(sk) => sk,
        None => return false,
    };
    value["name"].is_string()
        && value["minecraftVersion"].is_string()
        && value["atpcraftVersion"].is_string()
        && signing_key.get("keyId").is_some()
        && signing_key.get("publicKey").is_some()
        && signing_key.get("alg").is_some()
}

/// The structurally valid record at rkey `self` preferred, otherwise the
/// newest structurally valid record (created_at descending). Returns `None`
/// when the repo has no parseable server profile.
pub fn latest(records: &[(String, serde_json::Value)]) -> Option<(String, serde_json::Value)> {
    let mut self_record: Option<(String, serde_json::Value)> = None;
    let mut best: Option<(String, serde_json::Value)> = None;
    for (rkey, value) in records {
        if !structural_valid(value) {
            continue;
        }
        if rkey == records::SERVER_PROFILE_RKEY {
            self_record = Some((rkey.clone(), value.clone()));
        } else if best.is_none() {
            best = Some((rkey.clone(), value.clone()));
        } else if let Some((_, best_value)) = &best
            && value["updatedAt"].as_str() > best_value["updatedAt"].as_str()
        {
            best = Some((rkey.clone(), value.clone()));
        }
    }
    self_record.or(best)
}

/// Verify a server profile's `signingKey` block against `keys`: the key id
/// must name a trusted key whose public key matches the published one.
pub fn verify(record: &serde_json::Value, keys: &HashMap<String, VerifyingKey>) -> ServerVerdict {
    let signing_key = match record.get("signingKey") {
        Some(sk) => sk,
        None => return ServerVerdict::Malformed,
    };
    let key_id = match signing_key.get("keyId").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return ServerVerdict::Malformed,
    };
    let published = match signing_key.get("publicKey").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ServerVerdict::Malformed,
    };
    let Some(trusted_key) = keys.get(key_id) else {
        return ServerVerdict::Unknown;
    };
    // The published `publicKey` is the base64 (standard, padded) SPKI
    // encoding of the Ed25519 key; the trusted key carries the same encoding.
    if published == signing::public_key_spki_b64(trusted_key) {
        ServerVerdict::Trusted {
            key_id: key_id.to_string(),
        }
    } else {
        ServerVerdict::KeyMismatch {
            key_id: key_id.to_string(),
        }
    }
}

/// Build the HUD digest of a server profile record: identity fields, versions,
/// capabilities, operator handle, and the trust verdict.
pub fn digest(
    did: &str,
    rkey: &str,
    record: &serde_json::Value,
    keys: &HashMap<String, VerifyingKey>,
) -> Result<ServerDigest> {
    let signing_key = record
        .get("signingKey")
        .ok_or_else(|| AtmosphericError::Message("server profile has no signingKey".into()))?;
    let signing_key_id = signing_key
        .get("keyId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let operator_handle = record
        .get("operator")
        .and_then(|o| o.get("handle").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    Ok(ServerDigest {
        did: did.to_string(),
        rkey: rkey.to_string(),
        at_uri: format!("at://{did}/{}/{}", records::SERVER_PROFILE_COLLECTION, rkey),
        name: record
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        description: record
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        address: record
            .get("address")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        minecraft_version: record
            .get("minecraftVersion")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        atpcraft_version: record
            .get("atpcraftVersion")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        capabilities: record
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        operator_handle,
        created_at: record
            .get("createdAt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        updated_at: record
            .get("updatedAt")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        signing_key_id,
        verdict: verify(record, keys),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_value() -> serde_json::Value {
        serde_json::json!({
            "name": "Example Server",
            "description": "A test atpcraft server",
            "address": "play.example.com",
            "minecraftVersion": "26.3",
            "atpcraftVersion": "0.1.0",
            "signingKey": {
                "keyId": "trusted-key",
                // RFC 8032 Ed25519 test vector 1 public key in SPKI/base64 form —
                // a real 32-byte ed25519 key so verifying_key_from_spki_b64 succeeds.
                "publicKey": "MCowBQYDK2VwAyEA11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=",
                "alg": "ed25519"
            },
            "capabilities": ["player-data-sync", "skin-sync"],
            "operator": {
                "did": "did:plc:operator",
                "handle": "operator.example",
                "website": "https://example.com",
                "contact": "admin@example.com"
            },
            "createdAt": "2026-11-01T00:00:00.000Z",
            "updatedAt": "2026-11-12T00:00:00.000Z",
            "$type": records::SERVER_PROFILE_COLLECTION
        })
    }

    fn keys_of(key_id: &str, public_key_b64: &str) -> HashMap<String, VerifyingKey> {
        let mut keys = HashMap::new();
        if let Ok(k) = signing::verifying_key_from_spki_b64(public_key_b64) {
            keys.insert(key_id.to_string(), k);
        }
        keys
    }

    #[test]
    fn structural_valid_rejects_incomplete_records() {
        let mut value = record_value();
        assert!(structural_valid(&value));
        value.as_object_mut().unwrap().remove("signingKey");
        assert!(!structural_valid(&value));
        let mut value = record_value();
        value
            .as_object_mut()
            .unwrap()
            .get_mut("signingKey")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("keyId");
        assert!(!structural_valid(&value));
    }

    #[test]
    fn verify_reports_all_verdicts() {
        let record = record_value();
        // RFC 8032 Ed25519 test vector 1 — same key as the record's publicKey.
        let spki_v1 = "MCowBQYDK2VwAyEA11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
        let keys = keys_of("trusted-key", spki_v1);

        // Trusted: key_id in set, public key matches.
        assert_eq!(
            verify(&record, &keys),
            ServerVerdict::Trusted {
                key_id: "trusted-key".into()
            }
        );

        // Unknown: key_id not in set.
        let empty = HashMap::new();
        assert_eq!(verify(&record, &empty), ServerVerdict::Unknown);

        // KeyMismatch: key_id trusted but public key differs (RFC 8032 vector 2).
        let mut keys = keys.clone();
        let spki_v2 = "MCowBQYDK2VwAyEAPUAXw+hDiVqStwqnTRt+vJyYLM8uxJaMwM1V8Sr0Zgw=";
        if let Ok(k) = signing::verifying_key_from_spki_b64(spki_v2) {
            keys.insert("trusted-key".into(), k);
        }
        assert_eq!(
            verify(&record, &keys),
            ServerVerdict::KeyMismatch {
                key_id: "trusted-key".into()
            }
        );

        // Malformed: no signingKey block.
        let mut bad = record.clone();
        bad.as_object_mut().unwrap().remove("signingKey");
        assert_eq!(verify(&bad, &keys), ServerVerdict::Malformed);
    }

    #[test]
    fn latest_prefers_self_rkey() {
        let records = vec![
            ("z".to_string(), record_value()),
            ("self".to_string(), record_value()),
            ("a".to_string(), record_value()),
        ];
        let (rkey, _) = latest(&records).unwrap();
        assert_eq!(rkey, "self");
    }

    #[test]
    fn latest_skips_invalid_records() {
        let records = vec![
            (
                "bad".to_string(),
                serde_json::json!({"not": "a server profile"}),
            ),
            ("self".to_string(), record_value()),
        ];
        let (rkey, _) = latest(&records).unwrap();
        assert_eq!(rkey, "self");
    }

    #[test]
    fn latest_picks_newest_by_updated_at() {
        let mut v1 = record_value();
        v1["updatedAt"] = serde_json::json!("2026-11-10T00:00:00.000Z");
        let mut v2 = record_value();
        v2["updatedAt"] = serde_json::json!("2026-11-11T00:00:00.000Z");
        let records = vec![("a".to_string(), v1), ("b".to_string(), v2)];
        let (rkey, _) = latest(&records).unwrap();
        assert_eq!(rkey, "b");
    }

    #[test]
    fn digest_builds_the_hud_fields() {
        let record = record_value();
        // RFC 8032 Ed25519 test vector 1 — same key as the record's publicKey.
        let keys = keys_of(
            "trusted-key",
            "MCowBQYDK2VwAyEA11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=",
        );
        let d = digest("did:plc:alice", "self", &record, &keys).unwrap();
        assert_eq!(d.did, "did:plc:alice");
        assert_eq!(d.rkey, "self");
        assert_eq!(d.name, "Example Server");
        assert_eq!(d.address, "play.example.com");
        assert_eq!(d.minecraft_version, "26.3");
        assert_eq!(d.atpcraft_version, "0.1.0");
        assert_eq!(d.capabilities, vec!["player-data-sync", "skin-sync"]);
        assert_eq!(d.operator_handle, "operator.example");
        assert_eq!(d.signing_key_id, "trusted-key");
        assert_eq!(
            d.verdict,
            ServerVerdict::Trusted {
                key_id: "trusted-key".into()
            }
        );
        assert_eq!(
            d.at_uri,
            format!(
                "at://did:plc:alice/{}/self",
                records::SERVER_PROFILE_COLLECTION
            )
        );
    }
}
