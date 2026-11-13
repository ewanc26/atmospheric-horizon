//! Reading other players' `at.atpcraft.player.skin` records — the read side
//! of atpcraft's skin sync — plus downloading the referenced blobs via
//! `com.atproto.sync.getBlob`, reimplemented from the lexicon semantics only:
//! no code is copied from atpcraft (its repo carries no licence).
//!
//! A player's skin records live in *their own* repo, one per endorsing
//! server (the record key is the server's signing key id), so the read is:
//! resolve the account and its PDS (as the M13 presence read does), page
//! `com.atproto.repo.listRecords` for the skin collection unauthenticated,
//! take the record with the newest `timestamp`, and verify its Ed25519
//! endorsements against known keys over atpcraft's canonical payload
//! `{did, skinCid?, capeCid?, model, timestamp, server}`. The skin/cape blobs
//! are then fetched with `com.atproto.sync.getBlob` (also unauthenticated on
//! public repos); the worker only persists blobs of *verified* records.

use crate::error::{AtmosphericError, Result};
use crate::records::{self, EndorsementVerdict, SkinRecord};
use ed25519_dalek::VerifyingKey;
use std::collections::HashMap;

/// One referenced (or downloaded) blob, reduced to what the HUD shows (M14).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlobDigest {
    pub cid: String,
    pub mime_type: String,
    pub size: u64,
    /// Where the worker saved the bytes; `None` until downloaded (the worker
    /// only downloads verified records).
    pub path: Option<String>,
}

/// A player's latest skin record, reduced to what the HUD shows (M14).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkinDigest {
    pub did: String,
    /// The record key (the endorsing server's key id).
    pub rkey: String,
    /// The record's AT-URI.
    pub at_uri: String,
    pub model: String,
    pub timestamp: String,
    pub verdict: EndorsementVerdict,
    pub skin: Option<BlobDigest>,
    pub cape: Option<BlobDigest>,
}

/// Parse a raw record value as a skin record, the way atpcraft's `fromJson`
/// does: the model must be a known value and the timestamp a valid datetime
/// (an unknown model or missing field makes the record unreadable). An empty
/// endorsements array still parses — the verdict reports `Malformed`.
fn parse_skin(value: &serde_json::Value) -> Option<SkinRecord> {
    serde_json::from_value(value.clone()).ok()
}

/// The skin record with the newest `timestamp` among the parseable ones
/// (skin records have no version counter; the timestamp orders them).
pub fn latest_skin(records: &[(String, serde_json::Value)]) -> Option<(String, serde_json::Value)> {
    records
        .iter()
        .filter(|(_, value)| parse_skin(value).is_some())
        .max_by_key(|(_, value)| value["timestamp"].as_str().unwrap_or_default().to_string())
        .cloned()
}

/// The canonical bytes a skin endorsement signs, built from the raw record
/// JSON so foreign records verify exactly as atpcraft's `signablePayload`
/// builds them: canonical JSON of `{did, skinCid?, capeCid?, model,
/// timestamp, server}` — the asset keys are omitted entirely when absent.
pub fn skin_payload(did: &str, record: &serde_json::Value, server: &str) -> Result<Vec<u8>> {
    let timestamp = record["timestamp"]
        .as_str()
        .ok_or_else(|| AtmosphericError::Message("skin record has no timestamp".into()))?;
    let model = record["model"]
        .as_str()
        .ok_or_else(|| AtmosphericError::Message("skin record has no model".into()))?;
    let mut payload = serde_json::json!({
        "did": did,
        "model": model,
        "timestamp": timestamp,
        "server": server,
    });
    if let Some(cid) = record["skin"]["ref"]["$link"].as_str() {
        payload["skinCid"] = serde_json::Value::String(cid.to_string());
    }
    if let Some(cid) = record["cape"]["ref"]["$link"].as_str() {
        payload["capeCid"] = serde_json::Value::String(cid.to_string());
    }
    Ok(records::canonical_json(&payload).into_bytes())
}

/// Verify a skin record's endorsements against `keys` (key id → public key),
/// mirroring atpcraft's `PlayerSkinRecord.verify`.
pub fn verify_skin(
    did: &str,
    record: &serde_json::Value,
    keys: &HashMap<String, VerifyingKey>,
) -> EndorsementVerdict {
    records::verify_endorsements(&record["endorsements"], keys, |server| {
        skin_payload(did, record, server).ok()
    })
}

/// Download a blob's raw bytes via `com.atproto.sync.getBlob` (unauthenticated
/// on public repos). Returns the response's content type and the bytes.
pub async fn get_blob(
    http: &reqwest::Client,
    pds: &str,
    did: &str,
    cid: &str,
) -> Result<(String, Vec<u8>)> {
    let base = pds.trim_end_matches('/');
    let response = http
        .get(format!("{base}/xrpc/com.atproto.sync.getBlob"))
        .query(&[("did", did), ("cid", cid)])
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AtmosphericError::Message(format!(
            "getBlob for {cid} in {did} failed with HTTP {status}: {body}"
        )));
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = response.bytes().await.map_err(AtmosphericError::protocol)?;
    Ok((mime, bytes.to_vec()))
}

/// Reduce a raw skin record to the HUD digest (blob fields from the record;
/// `path` is filled in by the worker after a verified download).
pub fn digest(
    did: &str,
    rkey: &str,
    record: &serde_json::Value,
    keys: &HashMap<String, VerifyingKey>,
) -> Result<SkinDigest> {
    let skin = parse_skin(record)
        .ok_or_else(|| AtmosphericError::Message("not a structurally valid skin record".into()))?;
    let blob = |key: &str| {
        let value = &record[key];
        if value.is_null() {
            None
        } else {
            Some(BlobDigest {
                cid: value["ref"]["$link"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                mime_type: value["mimeType"].as_str().unwrap_or_default().to_string(),
                size: value["size"].as_u64().unwrap_or_default(),
                path: None,
            })
        }
    };
    Ok(SkinDigest {
        did: did.into(),
        rkey: rkey.into(),
        at_uri: format!("at://{did}/{}/{rkey}", records::SKIN_COLLECTION),
        model: skin.model.key().to_string(),
        timestamp: record["timestamp"].as_str().unwrap_or_default().to_string(),
        verdict: verify_skin(did, record, keys),
        skin: blob("skin"),
        cape: blob("cape"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{self, SigningKey};

    const SKIN_CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";
    const CAPE_CID: &str = "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy";

    fn blob_json(cid: &str) -> serde_json::Value {
        serde_json::json!({
            "$type": "blob",
            "ref": {"$link": cid},
            "mimeType": "image/png",
            "size": 1234,
        })
    }

    /// A skin record value with no endorsements yet.
    fn record_value(timestamp: &str) -> serde_json::Value {
        serde_json::json!({
            "$type": records::SKIN_COLLECTION,
            "skin": blob_json(SKIN_CID),
            "cape": blob_json(CAPE_CID),
            "model": "slim",
            "timestamp": timestamp,
            "endorsements": [],
        })
    }

    /// Sign `record` with `key`; returns (record with the endorsement, key id).
    fn signed(
        record: serde_json::Value,
        did: &str,
        key: &SigningKey,
    ) -> (serde_json::Value, String) {
        let key_id = signing::key_id_of(&key.verifying_key());
        let payload = skin_payload(did, &record, &key_id).unwrap();
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
    fn payload_matches_the_m9_writer() {
        // The raw-JSON builder must agree with SkinRecord::signable_payload
        // (the M9 writer) so records written by this mod verify either way.
        let record = records::SkinRecord {
            skin: Some(serde_json::from_value(blob_json(SKIN_CID)).unwrap()),
            cape: Some(serde_json::from_value(blob_json(CAPE_CID)).unwrap()),
            model: records::SkinModel::Slim,
            timestamp: "2026-11-12T12:00:00.000Z".parse().unwrap(),
            endorsements: vec![],
        };
        let raw = serde_json::to_value(&record).unwrap();
        assert_eq!(
            skin_payload("did:plc:alice", &raw, "key1").unwrap(),
            record.signable_payload("did:plc:alice", "key1"),
        );
        // And the asset keys are omitted when absent.
        let mut raw = raw.clone();
        raw.as_object_mut().unwrap().remove("cape");
        let payload =
            String::from_utf8(skin_payload("did:plc:alice", &raw, "key1").unwrap()).unwrap();
        assert!(!payload.contains("capeCid"), "{payload}");
        assert!(payload.contains(r#""skinCid":""#), "{payload}");
    }

    #[test]
    fn verify_skin_reports_all_verdicts() {
        let key = signing::generate();
        let did = "did:plc:alice";
        let (record, key_id) = signed(record_value("2026-11-12T12:00:00.000Z"), did, &key);
        assert_eq!(
            verify_skin(did, &record, &keys_of(&key)),
            EndorsementVerdict::Verified {
                endorser: key_id.clone()
            }
        );
        assert_eq!(
            verify_skin(did, &record, &HashMap::new()),
            EndorsementVerdict::UnknownEndorser
        );
        let mut tampered = record.clone();
        tampered["model"] = serde_json::json!("default"); // edited after signing
        assert_eq!(
            verify_skin(did, &tampered, &keys_of(&key)),
            EndorsementVerdict::BadSignature
        );
        // A different DID breaks the signature too.
        assert_eq!(
            verify_skin("did:plc:bob", &record, &keys_of(&key)),
            EndorsementVerdict::BadSignature
        );
        // No endorsements at all.
        assert_eq!(
            verify_skin(
                did,
                &record_value("2026-11-12T12:00:00.000Z"),
                &keys_of(&key)
            ),
            EndorsementVerdict::Malformed
        );
    }

    #[test]
    fn latest_skin_picks_the_newest_timestamp_and_skips_garbage() {
        let records = vec![
            ("k1".to_string(), record_value("2026-11-12T10:00:00.000Z")),
            ("k2".to_string(), record_value("2026-11-12T13:00:00.000Z")),
            ("k3".to_string(), serde_json::json!({"model": "wide"})), // unknown model
            ("k4".to_string(), record_value("2026-11-12T11:00:00.000Z")),
        ];
        let (rkey, value) = latest_skin(&records).unwrap();
        assert_eq!(rkey, "k2");
        assert_eq!(value["timestamp"], "2026-11-12T13:00:00.000Z");
        assert!(latest_skin(&[("k".to_string(), serde_json::json!({"model": "wide"}))]).is_none());
    }

    #[test]
    fn digest_reduces_the_record_and_carries_the_verdict() {
        let key = signing::generate();
        let did = "did:plc:alice";
        let (record, key_id) = signed(record_value("2026-11-12T12:00:00.000Z"), did, &key);
        let d = digest(did, &key_id, &record, &keys_of(&key)).unwrap();
        assert_eq!(d.did, did);
        assert_eq!(d.rkey, key_id);
        assert_eq!(
            d.at_uri,
            format!("at://{did}/{}/{key_id}", records::SKIN_COLLECTION)
        );
        assert_eq!(d.model, "slim");
        assert_eq!(d.timestamp, "2026-11-12T12:00:00.000Z");
        let skin = d.skin.as_ref().unwrap();
        assert_eq!(skin.cid, SKIN_CID);
        assert_eq!(skin.mime_type, "image/png");
        assert_eq!(skin.size, 1234);
        assert!(skin.path.is_none(), "the worker fills the path");
        assert_eq!(d.cape.as_ref().unwrap().cid, CAPE_CID);
        assert_eq!(d.verdict, EndorsementVerdict::Verified { endorser: key_id });
        // A record with only a skin has no cape entry at all.
        let mut raw = record_value("2026-11-12T12:00:00.000Z");
        raw.as_object_mut().unwrap().remove("cape");
        let (raw, _) = signed(raw, did, &key);
        let d = digest(did, "k", &raw, &keys_of(&key)).unwrap();
        assert!(d.skin.is_some());
        assert!(d.cape.is_none());
    }

    #[test]
    fn get_blob_returns_the_content_type_and_bytes() {
        // A stub PDS serving getBlob with a PNG body.
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
                        let png = b"\x89PNG\r\n\x1a\nrest-of-png".to_vec();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            png.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(&png);
                    });
                }
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (mime, bytes) = runtime.block_on(async {
            let http = reqwest::Client::new();
            get_blob(&http, &url, "did:plc:alice", SKIN_CID)
                .await
                .unwrap()
        });
        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].starts_with("/xrpc/com.atproto.sync.getBlob?"));
        assert!(hits[0].contains("did=did%3Aplc%3Aalice"));
        assert!(hits[0].contains(&format!("cid={SKIN_CID}")));
    }
}
