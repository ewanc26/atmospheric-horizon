//! Record types for the `at.atpcraft.*` lexicons, ported from atpcraft
//! (https://git.atforge.org/atforge/atpcraft/ATPcraft).
//!
//! atpcraft ships its lexicons as JSON schemas; those schema shapes are
//! reimplemented here as plain Rust data (atpcraft's repo carries no licence,
//! so no code is copied — only the wire format is matched, which is the point
//! of a lexicon). The collection IDs and field names match atpcraft's so
//! records written by either implementation are interoperable.

use crate::error::Result;
use atrium_api::types::{Unknown, string::Datetime};
use serde::{Deserialize, Serialize};

/// The `at.atpcraft.player.chat` collection: one record per chat message,
/// keyed by a TID so a player's messages accumulate as an append-only history.
pub const CHAT_COLLECTION: &str = "at.atpcraft.player.chat";

/// The `at.atpcraft.player.snapshot` collection: one server-endorseable
/// snapshot per player per endorsing key (the record key is the key id).
pub const SNAPSHOT_COLLECTION: &str = "at.atpcraft.player.snapshot";

/// The lexicon's `message` cap (maxLength 1024).
pub const MAX_CHAT_MESSAGE_LEN: usize = 1024;

/// One player chat message, shaped for the `at.atpcraft.player.chat` lexicon.
///
/// Chat records are not server-signed (the lexicon has no endorsement fields):
/// a plain value object published to the author's own repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRecord {
    /// When the message was sent (ISO 8601).
    pub sent_at: Datetime,
    /// The plain text of the chat message, truncated to the lexicon's cap.
    pub message: String,
    /// Identifier of the server where the message was sent.
    pub server: String,
}

impl ChatRecord {
    /// Build a record for `server` from `message` at `sent_at`, truncating the
    /// message to [`MAX_CHAT_MESSAGE_LEN`] characters (the lexicon's cap).
    pub fn new(server: &str, message: &str, sent_at: Datetime) -> Self {
        Self {
            sent_at,
            message: message.chars().take(MAX_CHAT_MESSAGE_LEN).collect(),
            server: server.into(),
        }
    }

    /// The record body to write to the PDS, with the `$type` create_record
    /// requires (generated record types do not carry one themselves).
    pub fn to_unknown(&self) -> Result<Unknown> {
        crate::actions::record_to_unknown(CHAT_COLLECTION, self)
    }
}

/// Deterministic JSON for signing payloads, matching atpcraft's
/// `canonicalJson`: object keys recursively sorted (byte-wise; all our keys
/// are ASCII, so this matches), no insignificant whitespace, and strings
/// escaped only for `\"` and `\\` (atpcraft does not escape control chars).
pub fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let body = entries
                .into_iter()
                .map(|(k, v)| format!("\"{}\":{}", escape_key(k), canonical_json(v)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        serde_json::Value::Array(items) => {
            let body = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{body}]")
        }
        serde_json::Value::String(s) => format!("\"{}\"", escape_key(s)),
        other => other.to_string(),
    }
}

/// atpcraft's string escaping: only backslash and double quote.
fn escape_key(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `at.atpcraft.player.defs#position`: floored block coordinates (the atproto
/// data model has no floats).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

/// `at.atpcraft.player.defs#score`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    pub objective: String,
    pub value: i64,
}

/// `at.atpcraft.player.defs#teamSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// `at.atpcraft.player.defs#summary`: the human-readable part of a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSummary {
    pub username: String,
    pub dimension: String,
    pub position: Position,
    /// Health as a decimal string (the atproto data model has no floats).
    pub health: String,
    pub food: i64,
    pub xp_level: i64,
    pub game_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scores: Option<Vec<Score>>,
}

/// `at.atpcraft.player.defs#aspect`: one opaque payload, uploaded as a blob;
/// the record holds its reference. (atpcraft also supports a legacy `inline`
/// bytes form, which this port does not write.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Aspect {
    pub name: String,
    pub blob: atrium_api::types::BlobRef,
}

/// A `bytes` field as the wire format wants it: `{"$bytes": "<base64>"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BytesB64 {
    #[serde(rename = "$bytes")]
    pub value: String,
}

/// `at.atpcraft.player.snapshot#endorsement`: one server's Ed25519 signature
/// over the canonical payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endorsement {
    /// The endorsing key id (also the record key).
    pub server: String,
    pub alg: String,
    pub sig: BytesB64,
}

/// One `at.atpcraft.player.snapshot` record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRecord {
    /// Monotonic version (anti-rollback).
    pub version: i64,
    pub issued_at: Datetime,
    pub summary: SnapshotSummary,
    /// Name-sorted, names unique.
    pub aspects: Vec<Aspect>,
    pub endorsements: Vec<Endorsement>,
}

impl SnapshotRecord {
    /// The record body to write to the PDS, with the `$type` create_record
    /// requires.
    pub fn to_unknown(&self) -> Result<Unknown> {
        crate::actions::record_to_unknown(SNAPSHOT_COLLECTION, self)
    }

    /// The canonical bytes an endorsement signs, matching atpcraft's
    /// `signablePayload`: canonical JSON of `{did, version, issuedAt, server,
    /// summary, aspects}` with aspects name-sorted as `{name, blob: cid}`.
    pub fn signable_payload(&self, did: &str, server: &str) -> Vec<u8> {
        let mut aspects: Vec<serde_json::Value> = self
            .aspects
            .iter()
            .map(|a| {
                // The blob's CID as the `$link` string, as atpcraft signs it.
                let cid = serde_json::to_value(&a.blob)
                    .ok()
                    .and_then(|v| v.get("ref").and_then(|r| r.get("$link")).cloned())
                    .unwrap_or_default();
                serde_json::json!({
                    "name": a.name,
                    "blob": cid,
                })
            })
            .collect();
        aspects.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let payload = serde_json::json!({
            "did": did,
            "version": self.version,
            "issuedAt": self.issued_at,
            "server": server,
            "summary": self.summary,
            "aspects": aspects,
        });
        canonical_json(&payload).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_and_escapes_only_quote_and_backslash() {
        let value = serde_json::json!({
            "zeta": 1,
            "alpha": {"b": true, "a": [1, "x\"y\\z"]},
        });
        assert_eq!(
            canonical_json(&value),
            r#"{"alpha":{"a":[1,"x\"y\\z"],"b":true},"zeta":1}"#
        );
    }

    #[test]
    fn chat_record_serialises_with_camel_case_fields_and_dollar_type() {
        let record = ChatRecord::new(
            "test-server",
            "hello world",
            "2026-11-12T12:00:00.000Z".parse().unwrap(),
        );
        let unknown = record.to_unknown().unwrap();
        let value = serde_json::to_value(&unknown).unwrap();
        assert_eq!(value["$type"], CHAT_COLLECTION);
        assert_eq!(value["sentAt"], "2026-11-12T12:00:00.000Z");
        assert_eq!(value["message"], "hello world");
        assert_eq!(value["server"], "test-server");
    }

    #[test]
    fn chat_record_truncates_the_message_to_the_lexicon_cap() {
        let long = "x".repeat(MAX_CHAT_MESSAGE_LEN + 100);
        let record = ChatRecord::new("s", &long, Datetime::now());
        assert_eq!(record.message.chars().count(), MAX_CHAT_MESSAGE_LEN);
    }

    fn example_summary() -> SnapshotSummary {
        SnapshotSummary {
            username: "Steve".into(),
            dimension: "minecraft:overworld".into(),
            position: Position {
                x: -25,
                y: 67,
                z: -3,
            },
            health: "19.5".into(),
            food: 20,
            xp_level: 3,
            game_mode: "survival".into(),
            team: None,
            scores: Some(vec![Score {
                objective: "deaths".into(),
                value: 2,
            }]),
        }
    }

    #[test]
    fn snapshot_record_serialises_with_dollar_type_and_camel_case() {
        let record = SnapshotRecord {
            version: 7,
            issued_at: "2026-11-12T12:00:00.000Z".parse().unwrap(),
            summary: example_summary(),
            aspects: vec![],
            endorsements: vec![Endorsement {
                server: "abc123".into(),
                alg: "ed25519".into(),
                sig: BytesB64 {
                    value: "c2ln".into(),
                },
            }],
        };
        let value = serde_json::to_value(record.to_unknown().unwrap()).unwrap();
        assert_eq!(value["$type"], SNAPSHOT_COLLECTION);
        assert_eq!(value["version"], 7);
        assert_eq!(value["issuedAt"], "2026-11-12T12:00:00.000Z");
        assert_eq!(value["summary"]["username"], "Steve");
        assert_eq!(value["summary"]["xpLevel"], 3);
        assert_eq!(value["summary"]["position"]["x"], -25);
        assert_eq!(value["summary"]["scores"][0]["objective"], "deaths");
        assert_eq!(value["endorsements"][0]["alg"], "ed25519");
        assert_eq!(value["endorsements"][0]["sig"]["$bytes"], "c2ln");
        assert_eq!(value["aspects"], serde_json::json!([]));
    }

    #[test]
    fn signable_payload_is_canonical_and_binds_the_essential_fields() {
        let record = SnapshotRecord {
            version: 2,
            issued_at: "2026-11-12T12:00:00.000Z".parse().unwrap(),
            summary: example_summary(),
            aspects: vec![],
            endorsements: vec![],
        };
        let payload = String::from_utf8(record.signable_payload("did:plc:alice", "key1")).unwrap();
        // Keys sorted, no whitespace; the payload binds did, version, server.
        assert_eq!(
            payload,
            r#"{"aspects":[],"did":"did:plc:alice","issuedAt":"2026-11-12T12:00:00.000Z","server":"key1","summary":{"dimension":"minecraft:overworld","food":20,"gameMode":"survival","health":"19.5","position":{"x":-25,"y":67,"z":-3},"scores":[{"objective":"deaths","value":2}],"username":"Steve","xpLevel":3},"version":2}"#
        );
        // A different did or server changes the payload (and breaks the sig).
        assert_ne!(
            record.signable_payload("did:plc:alice", "key1"),
            record.signable_payload("did:plc:bob", "key1")
        );
    }
}
