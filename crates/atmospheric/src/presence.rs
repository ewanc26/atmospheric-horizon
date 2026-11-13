//! Reading other players' `at.atpcraft.player.snapshot` records — the read
//! side of atpcraft's player-state sync ("presence"), reimplemented from the
//! lexicon semantics only: no code is copied from atpcraft (its repo carries
//! no licence).
//!
//! A player's snapshots live in *their own* repo, one record per endorsing
//! server (the record key is the server's signing key id), so the read is:
//! resolve the account (handle → DID via the same directory service OAuth
//! login uses, then the DID document's `#atproto_pds` endpoint), page
//! `com.atproto.repo.listRecords` for the snapshot collection
//! **unauthenticated** against that PDS, take the highest `version`, and
//! verify its Ed25519 endorsements against known keys — the mod's own signing
//! key plus the `trusted_server_keys` settings list — over the same canonical
//! payload atpcraft signs (`{did, version, issuedAt, server, summary,
//! aspects}`, aspects name-sorted as `{name, blob: cid}` or
//! `{name, inline: base64url}`).

use crate::error::{AtmosphericError, Result};
use crate::records::{self, EndorsementVerdict};
use base64::{Engine as _, engine::general_purpose};
use ed25519_dalek::VerifyingKey;
use std::collections::HashMap;

/// `com.atproto.repo.listRecords` page size.
pub const LIST_LIMIT: u32 = 100;

/// Defensive pagination cap (100 pages = 10 000 records).
pub const MAX_PAGES: usize = 100;

/// What the HUD shows for a player's latest snapshot (M13).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlayerDigest {
    pub did: String,
    /// The record key (the endorsing server's key id).
    pub rkey: String,
    /// The record's AT-URI.
    pub at_uri: String,
    pub version: i64,
    pub issued_at: String,
    pub username: String,
    pub dimension: String,
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub health: String,
    pub food: i64,
    pub xp_level: i64,
    pub game_mode: String,
    pub verdict: EndorsementVerdict,
}

/// Resolve a player reference (a DID or handle — not a record URI) to the
/// account's DID and the PDS endpoint its DID document advertises.
pub async fn resolve_player(
    http: &reqwest::Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<(String, String)> {
    let reference = crate::labels::parse_subject_reference(raw)?;
    let resolved = crate::labels::resolve_subject(http, resolver, &reference).await?;
    if resolved.collection.is_some() {
        return Err(AtmosphericError::Message(
            "want a player account (a DID or a handle), not a record URI".into(),
        ));
    }
    // For an account subject the pattern is the bare DID.
    let did = resolved.pattern;
    let doc = crate::oauth::resolve_did_document(http, resolver, &did).await?;
    let pds = crate::oauth::pds_endpoint(&doc)?;
    Ok((did, pds))
}

/// Page `com.atproto.repo.listRecords` for `collection` in `repo`'s own PDS,
/// unauthenticated (public repos serve this). Returns `(rkey, value)` pairs
/// in page order.
pub async fn list_records(
    http: &reqwest::Client,
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

/// The record's version if the value is structurally a snapshot record:
/// `version`/`issuedAt`/`summary`/`aspects`/`endorsements` present, aspect
/// names sorted and unique, each aspect a blob or inline payload — the
/// canonical form atpcraft's `fromJson` enforces. `None` otherwise.
fn structural_version(value: &serde_json::Value) -> Option<i64> {
    let version = value["version"].as_i64()?;
    value["issuedAt"].as_str()?;
    value["summary"].as_object()?;
    let aspects = value["aspects"].as_array()?;
    let mut previous: Option<&str> = None;
    for aspect in aspects {
        let name = aspect["name"].as_str()?;
        if let Some(prev) = previous
            && name <= prev
        {
            return None; // unsorted or duplicate
        }
        if aspect.get("blob").is_none() && aspect.get("inline").is_none() {
            return None;
        }
        previous = Some(name);
    }
    value["endorsements"].as_array()?;
    Some(version)
}

/// The structurally valid record with the highest version (the anti-rollback
/// counter), if any.
pub fn latest(records: &[(String, serde_json::Value)]) -> Option<(String, serde_json::Value)> {
    let mut best: Option<(i64, String, serde_json::Value)> = None;
    for (rkey, value) in records {
        if let Some(version) = structural_version(value)
            && best.as_ref().is_none_or(|(v, _, _)| version > *v)
        {
            best = Some((version, rkey.clone(), value.clone()));
        }
    }
    best.map(|(_, rkey, value)| (rkey, value))
}

/// The canonical bytes an endorsement signs, built from the raw record JSON
/// so foreign records (including `inline` aspects) verify exactly as
/// atpcraft's `signablePayload` builds them.
pub fn snapshot_payload(did: &str, record: &serde_json::Value, server: &str) -> Result<Vec<u8>> {
    let version = record["version"]
        .as_i64()
        .ok_or_else(|| AtmosphericError::Message("snapshot record has no version".into()))?;
    let issued_at = record["issuedAt"]
        .as_str()
        .ok_or_else(|| AtmosphericError::Message("snapshot record has no issuedAt".into()))?;
    let summary = record["summary"].clone();
    let mut aspects: Vec<serde_json::Value> = Vec::new();
    if let Some(list) = record["aspects"].as_array() {
        for aspect in list {
            let name = aspect["name"]
                .as_str()
                .ok_or_else(|| AtmosphericError::Message("aspect has no name".into()))?;
            let entry = if let Some(blob) = aspect.get("blob") {
                let cid = blob["ref"]["$link"]
                    .as_str()
                    .ok_or_else(|| AtmosphericError::Message("aspect blob has no CID".into()))?;
                serde_json::json!({ "name": name, "blob": cid })
            } else if let Some(inline) = aspect.get("inline") {
                let bytes_b64 = inline["$bytes"].as_str().ok_or_else(|| {
                    AtmosphericError::Message("inline aspect has no bytes".into())
                })?;
                let bytes = records::decode_b64_flexible(bytes_b64).ok_or_else(|| {
                    AtmosphericError::Message("inline aspect bytes are not base64".into())
                })?;
                // atpcraft signs inline bytes as base64url without padding.
                serde_json::json!({ "name": name, "inline": general_purpose::URL_SAFE_NO_PAD.encode(bytes) })
            } else {
                return Err(AtmosphericError::Message(
                    "aspect has neither blob nor inline payload".into(),
                ));
            };
            aspects.push(entry);
        }
    }
    aspects.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let payload = serde_json::json!({
        "did": did,
        "version": version,
        "issuedAt": issued_at,
        "server": server,
        "summary": summary,
        "aspects": aspects,
    });
    Ok(records::canonical_json(&payload).into_bytes())
}

/// Verify a snapshot record's endorsements against `keys` (key id → public
/// key), mirroring atpcraft's `PlayerSnapshotRecord.verify`: any endorsement
/// from a known key that verifies is enough.
pub fn verify(
    did: &str,
    record: &serde_json::Value,
    keys: &HashMap<String, VerifyingKey>,
) -> EndorsementVerdict {
    records::verify_endorsements(&record["endorsements"], keys, |server| {
        snapshot_payload(did, record, server).ok()
    })
}

/// Build the HUD digest of a snapshot record: the summary fields plus the
/// verification verdict.
pub fn digest(
    did: &str,
    rkey: &str,
    record: &serde_json::Value,
    keys: &HashMap<String, VerifyingKey>,
) -> Result<PlayerDigest> {
    let version = structural_version(record).ok_or_else(|| {
        AtmosphericError::Message("not a structurally valid snapshot record".into())
    })?;
    let summary = &record["summary"];
    let position = &summary["position"];
    Ok(PlayerDigest {
        did: did.into(),
        rkey: rkey.into(),
        at_uri: format!("at://{did}/{}/{rkey}", records::SNAPSHOT_COLLECTION),
        version,
        issued_at: record["issuedAt"].as_str().unwrap_or_default().to_string(),
        username: summary["username"].as_str().unwrap_or_default().to_string(),
        dimension: summary["dimension"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        x: position["x"].as_i64().unwrap_or_default(),
        y: position["y"].as_i64().unwrap_or_default(),
        z: position["z"].as_i64().unwrap_or_default(),
        health: summary["health"].as_str().unwrap_or_default().to_string(),
        food: summary["food"].as_i64().unwrap_or_default(),
        xp_level: summary["xpLevel"].as_i64().unwrap_or_default(),
        game_mode: summary["gameMode"].as_str().unwrap_or_default().to_string(),
        verdict: verify(did, record, keys),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{self, SigningKey};

    const SKIN_CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";

    fn summary() -> serde_json::Value {
        serde_json::json!({
            "username": "Steve",
            "dimension": "minecraft:overworld",
            "position": {"x": -25, "y": 67, "z": -3},
            "health": "19.5",
            "food": 20,
            "xpLevel": 3,
            "gameMode": "survival",
        })
    }

    /// A blob-aspect snapshot record value with no endorsements yet.
    fn record_value(version: i64, aspects: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "$type": records::SNAPSHOT_COLLECTION,
            "version": version,
            "issuedAt": "2026-11-12T12:00:00.000Z",
            "summary": summary(),
            "aspects": aspects,
            "endorsements": [],
        })
    }

    fn blob_aspect() -> serde_json::Value {
        serde_json::json!({
            "name": "core",
            "blob": {"$type": "blob", "ref": {"$link": SKIN_CID}, "mimeType": "application/octet-stream", "size": 42},
        })
    }

    /// Sign `record` with `key` and return (record with the endorsement, key id).
    fn signed(
        record: serde_json::Value,
        did: &str,
        key: &SigningKey,
    ) -> (serde_json::Value, String) {
        let key_id = signing::key_id_of(&key.verifying_key());
        let payload = snapshot_payload(did, &record, &key_id).unwrap();
        let sig = signing::sign_b64(key, &payload);
        let mut record = record;
        record["endorsements"] = serde_json::json!([{
            "server": key_id,
            "alg": "ed25519",
            "sig": {"$bytes": sig},
        }]);
        (record, key_id)
    }

    fn keys_of(key: &SigningKey) -> HashMap<String, VerifyingKey> {
        HashMap::from([(
            signing::key_id_of(&key.verifying_key()),
            key.verifying_key(),
        )])
    }

    #[test]
    fn payload_matches_the_m6_writer_for_blob_aspects() {
        // The raw-JSON builder must agree with SnapshotRecord::signable_payload
        // (the M6 writer) so records written by this mod verify either way.
        let record = records::SnapshotRecord {
            version: 2,
            issued_at: "2026-11-12T12:00:00.000Z".parse().unwrap(),
            summary: serde_json::from_value(summary()).unwrap(),
            aspects: vec![records::Aspect {
                name: "core".into(),
                blob: serde_json::from_value(serde_json::json!({
                    "$type": "blob",
                    "ref": {"$link": SKIN_CID},
                    "mimeType": "application/octet-stream",
                    "size": 42,
                }))
                .unwrap(),
            }],
            endorsements: vec![],
        };
        let raw = serde_json::to_value(&record).unwrap();
        assert_eq!(
            snapshot_payload("did:plc:alice", &raw, "key1").unwrap(),
            record.signable_payload("did:plc:alice", "key1"),
        );
    }

    #[test]
    fn payload_binds_inline_aspects_as_base64url() {
        let record = record_value(
            1,
            serde_json::json!([{
                "name": "small",
                "inline": {"$bytes": general_purpose::STANDARD_NO_PAD.encode(b"hello")},
            }]),
        );
        let payload =
            String::from_utf8(snapshot_payload("did:plc:alice", &record, "k").unwrap()).unwrap();
        // "hello" as base64url without padding.
        assert!(payload.contains(r#""inline":"aGVsbG8""#), "{payload}");
        assert!(payload.contains(r#""did":"did:plc:alice""#), "{payload}");
    }

    #[test]
    fn verify_reports_all_verdicts() {
        let key = signing::generate();
        let did = "did:plc:alice";
        let (record, key_id) = signed(
            record_value(3, serde_json::json!([blob_aspect()])),
            did,
            &key,
        );

        // A known key that verifies.
        assert_eq!(
            verify(did, &record, &keys_of(&key)),
            EndorsementVerdict::Verified {
                endorser: key_id.clone()
            }
        );
        // No known keys at all.
        assert_eq!(
            verify(did, &record, &HashMap::new()),
            EndorsementVerdict::UnknownEndorser
        );
        // A known key whose signature does not match (the record was edited).
        let mut tampered = record.clone();
        tampered["summary"]["food"] = serde_json::json!(0);
        assert_eq!(
            verify(did, &tampered, &keys_of(&key)),
            EndorsementVerdict::BadSignature
        );
        // A signature over a different DID does not verify either.
        assert_eq!(
            verify("did:plc:bob", &record, &keys_of(&key)),
            EndorsementVerdict::BadSignature
        );
        // No endorsements at all.
        assert_eq!(
            verify(did, &record_value(1, serde_json::json!([])), &keys_of(&key)),
            EndorsementVerdict::Malformed
        );
        // A foreign algorithm is skipped; with no other endorsements the
        // verdict is "unknown" (atpcraft's behaviour).
        let mut other_alg = record.clone();
        other_alg["endorsements"] =
            serde_json::json!([{"server": key_id, "alg": "p256", "sig": {"$bytes": "eA"}}]);
        assert_eq!(
            verify(did, &other_alg, &keys_of(&key)),
            EndorsementVerdict::UnknownEndorser
        );
    }

    #[test]
    fn inline_aspect_records_verify_end_to_end() {
        let key = signing::generate();
        let did = "did:plc:alice";
        let record = record_value(
            5,
            serde_json::json!([{
                "name": "small",
                "inline": {"$bytes": general_purpose::STANDARD_NO_PAD.encode(b"hello")},
            }]),
        );
        let (record, _) = signed(record, did, &key);
        assert_eq!(
            verify(did, &record, &keys_of(&key)),
            EndorsementVerdict::Verified {
                endorser: signing::key_id_of(&key.verifying_key())
            }
        );
    }

    #[test]
    fn structural_parse_rejects_unsorted_or_duplicate_aspects() {
        let unsorted = record_value(
            1,
            serde_json::json!([blob_aspect(), {"name": "aaa", "blob": {"ref": {"$link": SKIN_CID}}}]),
        );
        assert!(structural_version(&unsorted).is_none());
        let duplicate = record_value(1, serde_json::json!([blob_aspect(), blob_aspect()]));
        assert!(structural_version(&duplicate).is_none());
        assert_eq!(
            structural_version(&record_value(9, serde_json::json!([blob_aspect()]))),
            Some(9)
        );
    }

    #[test]
    fn latest_picks_the_highest_version_and_skips_garbage() {
        let records = vec![
            (
                "k1".to_string(),
                record_value(2, serde_json::json!([blob_aspect()])),
            ),
            (
                "k2".to_string(),
                record_value(7, serde_json::json!([blob_aspect()])),
            ),
            ("k3".to_string(), serde_json::json!({"nope": true})),
            (
                "k4".to_string(),
                record_value(5, serde_json::json!([blob_aspect()])),
            ),
        ];
        let (rkey, value) = latest(&records).unwrap();
        assert_eq!(rkey, "k2");
        assert_eq!(value["version"], 7);
        assert!(latest(&[("k".to_string(), serde_json::json!({"nope": 1}))]).is_none());
    }

    #[test]
    fn digest_reduces_the_summary_and_carries_the_verdict() {
        let key = signing::generate();
        let did = "did:plc:alice";
        let (record, key_id) = signed(
            record_value(4, serde_json::json!([blob_aspect()])),
            did,
            &key,
        );
        let d = digest(did, &key_id, &record, &keys_of(&key)).unwrap();
        assert_eq!(d.did, did);
        assert_eq!(d.rkey, key_id);
        assert_eq!(
            d.at_uri,
            format!("at://{did}/{}/{key_id}", records::SNAPSHOT_COLLECTION)
        );
        assert_eq!(d.version, 4);
        assert_eq!(d.username, "Steve");
        assert_eq!(d.dimension, "minecraft:overworld");
        assert_eq!((d.x, d.y, d.z), (-25, 67, -3));
        assert_eq!(d.health, "19.5");
        assert_eq!(d.food, 20);
        assert_eq!(d.xp_level, 3);
        assert_eq!(d.game_mode, "survival");
        assert_eq!(d.verdict, EndorsementVerdict::Verified { endorser: key_id });
        assert!(digest(did, "k", &serde_json::json!({}), &HashMap::new()).is_err());
    }

    #[test]
    fn list_records_paginates_until_the_cursor_ends() {
        // A two-page stub PDS serving listRecords.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let hits = std::sync::Arc::clone(&hits);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let hits = std::sync::Arc::clone(&hits);
                    std::thread::spawn(move || {
                        use std::io::{Read, Write};
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let _ = stream.read(&mut chunk);
                        buf.extend_from_slice(&chunk);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                        hits.lock().unwrap().push(path.clone());
                        let payload = if path.contains("cursor=c2") {
                            r#"{"records":[{"uri":"at://did:plc:alice/at.atpcraft.player.snapshot/k3","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","value":{"version":3}}]}"#
                        } else {
                            r#"{"records":[{"uri":"at://did:plc:alice/at.atpcraft.player.snapshot/k1","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","value":{"version":1}},{"uri":"at://did:plc:alice/at.atpcraft.player.snapshot/k2","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","value":{"version":2}}],"cursor":"c2"}"#
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    });
                }
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let records = runtime.block_on(async {
            let http = reqwest::Client::new();
            list_records(&http, &url, "did:plc:alice", records::SNAPSHOT_COLLECTION)
                .await
                .unwrap()
        });
        assert_eq!(
            records
                .iter()
                .map(|(rkey, value)| (rkey.as_str(), value["version"].as_i64().unwrap()))
                .collect::<Vec<_>>(),
            vec![("k1", 1), ("k2", 2), ("k3", 3)]
        );
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 2, "two pages were fetched: {hits:?}");
        assert!(hits[0].contains("repo=did%3Aplc%3Aalice"));
        assert!(hits[0].contains("collection=at.atpcraft.player.snapshot"));
        assert!(hits[0].contains("limit=100"));
        assert!(!hits[0].contains("cursor="));
        assert!(hits[1].contains("cursor=c2"));
    }
}
