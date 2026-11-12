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

/// The `at.atpcraft.player.skin` collection: one server-endorsed skin record
/// per player per endorsing key (the record key is the key id). Written with
/// `putRecord` (upsert), matching atpcraft (M9).
pub const SKIN_COLLECTION: &str = "at.atpcraft.player.skin";

/// The lexicon's blob cap for skins and capes (`maxSize` 1048576).
pub const MAX_SKIN_BYTES: usize = 1_048_576;

/// The `at.atpcraft.server.profile` collection: the account's self-published
/// server descriptor, a singleton at the literal rkey `self` (M10).
pub const SERVER_PROFILE_COLLECTION: &str = "at.atpcraft.server.profile";

/// The fixed record key of the server-profile singleton.
pub const SERVER_PROFILE_RKEY: &str = "self";

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

/// Minecraft skin model: `default` (classic) or `slim` arms, as the
/// `at.atpcraft.player.skin` lexicon's `knownValues` spell them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkinModel {
    Default,
    Slim,
}

impl SkinModel {
    /// The wire value used in records.
    pub fn key(self) -> &'static str {
        match self {
            SkinModel::Default => "default",
            SkinModel::Slim => "slim",
        }
    }

    /// Parse a wire value, rejecting unknown ones (atpcraft's `fromKey`).
    pub fn from_key(key: &str) -> Option<Self> {
        match key.to_ascii_lowercase().as_str() {
            "default" => Some(SkinModel::Default),
            "slim" => Some(SkinModel::Slim),
            _ => None,
        }
    }
}

/// The blob's CID as the `$link` string, for signing payloads.
pub fn blob_cid(blob: &atrium_api::types::BlobRef) -> Option<String> {
    serde_json::to_value(blob)
        .ok()?
        .get("ref")?
        .get("$link")?
        .as_str()
        .map(str::to_owned)
}

/// One `at.atpcraft.player.skin` record (M9): blob references to the skin and
/// optional cape held in the player's own repo, plus server endorsements.
/// Field names match the lexicon; blobs are absent (not null) when unset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkinRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skin: Option<atrium_api::types::BlobRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cape: Option<atrium_api::types::BlobRef>,
    pub model: SkinModel,
    /// When the record was written (ISO 8601).
    pub timestamp: Datetime,
    /// Server signatures over the canonical payload (min 1 per the lexicon).
    pub endorsements: Vec<Endorsement>,
}

impl SkinRecord {
    /// The record body to write to the PDS, with the `$type` put_record
    /// requires.
    pub fn to_unknown(&self) -> Result<Unknown> {
        crate::actions::record_to_unknown(SKIN_COLLECTION, self)
    }

    /// The canonical bytes an endorsement signs, matching atpcraft's
    /// `signablePayload`: canonical JSON of `{did, skinCid?, capeCid?,
    /// model, timestamp, server}` — blob CIDs only, and the asset keys are
    /// omitted entirely when the asset is absent.
    pub fn signable_payload(&self, did: &str, server: &str) -> Vec<u8> {
        let mut payload = serde_json::json!({
            "did": did,
            "model": self.model.key(),
            "timestamp": self.timestamp,
            "server": server,
        });
        if let Some(cid) = self.skin.as_ref().and_then(blob_cid) {
            payload["skinCid"] = serde_json::Value::String(cid);
        }
        if let Some(cid) = self.cape.as_ref().and_then(blob_cid) {
            payload["capeCid"] = serde_json::Value::String(cid);
        }
        canonical_json(&payload).into_bytes()
    }
}

/// The `signingKey` sub-object of a server profile: the Ed25519 endorsement
/// public key (base64-standard SPKI) so consumers can resolve player-record
/// endorsement key ids without out-of-band key sharing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSigningKey {
    pub key_id: String,
    /// Base64 (standard, padded) of the X.509/SPKI encoding.
    pub public_key: String,
    pub alg: String,
}

/// The optional `operator` sub-object; blank fields are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProfileOperator {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub did: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub website: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contact: String,
}

impl ProfileOperator {
    /// The operator object is omitted entirely when every field is blank.
    fn is_empty(&self) -> bool {
        self.did.is_empty()
            && self.handle.is_empty()
            && self.website.is_empty()
            && self.contact.is_empty()
    }
}

/// One `at.atpcraft.server.profile` record (M10): the account's self-published
/// server descriptor. Blank/empty optionals are omitted from the wire body,
/// matching atpcraft's `toJson`. The record itself carries no signature — it
/// lives in the account's own repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProfileRecord {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
    pub minecraft_version: String,
    pub atpcraft_version: String,
    pub signing_key: ProfileSigningKey,
    /// Sorted, unique (the lexicon requires it); omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<ProfileOperator>,
    /// Preserved from the existing record on re-publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub updated_at: Datetime,
}

impl ServerProfileRecord {
    /// Build a record, sorting/deduping capabilities as the lexicon requires.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        description: String,
        address: String,
        minecraft_version: String,
        atpcraft_version: String,
        signing_key: ProfileSigningKey,
        capabilities: Vec<String>,
        operator: Option<ProfileOperator>,
        created_at: Option<String>,
        updated_at: Datetime,
    ) -> Self {
        let mut capabilities = capabilities;
        capabilities.sort();
        capabilities.dedup();
        Self {
            name,
            description,
            address,
            minecraft_version,
            atpcraft_version,
            signing_key,
            capabilities,
            operator: operator.filter(|o| !o.is_empty()),
            created_at,
            updated_at,
        }
    }

    /// The record body to write to the PDS, with the `$type` put_record
    /// requires.
    pub fn to_unknown(&self) -> Result<Unknown> {
        crate::actions::record_to_unknown(SERVER_PROFILE_COLLECTION, self)
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

    // Two real CIDs (CidLink parses them); skin and cape get distinct ones.
    const SKIN_CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";
    const CAPE_CID: &str = "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy";

    fn stub_blob(cid: &str) -> atrium_api::types::BlobRef {
        serde_json::from_value(serde_json::json!({
            "$type": "blob",
            "ref": {"$link": cid},
            "mimeType": "image/png",
            "size": 1234,
        }))
        .unwrap()
    }

    #[test]
    fn skin_record_signable_payload_matches_atpcraft_canonical_form() {
        let record = SkinRecord {
            skin: Some(stub_blob(SKIN_CID)),
            cape: Some(stub_blob(CAPE_CID)),
            model: SkinModel::Slim,
            timestamp: "2026-11-12T12:00:00.000Z".parse().unwrap(),
            endorsements: vec![],
        };
        // atpcraft signs canonical JSON of {did, skinCid, capeCid, model,
        // timestamp, server} with keys sorted byte-wise.
        let payload =
            String::from_utf8(record.signable_payload("did:plc:stubuser", "serverkeyid0000001"))
                .unwrap();
        assert_eq!(
            payload,
            format!(
                r#"{{"capeCid":"{CAPE_CID}","did":"did:plc:stubuser","model":"slim","server":"serverkeyid0000001","skinCid":"{SKIN_CID}","timestamp":"2026-11-12T12:00:00.000Z"}}"#
            )
        );
    }

    #[test]
    fn skin_record_omits_absent_assets_from_the_signable_payload() {
        let record = SkinRecord {
            skin: None,
            cape: Some(stub_blob(CAPE_CID)),
            model: SkinModel::Default,
            timestamp: "2026-11-12T12:00:00.000Z".parse().unwrap(),
            endorsements: vec![],
        };
        let payload = String::from_utf8(record.signable_payload("did:plc:x", "k")).unwrap();
        assert!(!payload.contains("skinCid"), "{payload}");
        assert!(
            payload.contains(&format!(r#""capeCid":"{CAPE_CID}""#)),
            "{payload}"
        );
        assert!(payload.contains(r#""model":"default""#), "{payload}");

        // The record body itself omits absent assets (not null) and carries
        // the collection as $type; blobs keep their "blob" $type marker.
        let value = serde_json::to_value(record.to_unknown().unwrap()).unwrap();
        assert_eq!(value["$type"], SKIN_COLLECTION);
        assert!(value.get("skin").is_none());
        assert_eq!(value["cape"]["$type"], "blob");
        assert_eq!(value["cape"]["ref"]["$link"], CAPE_CID);
    }

    #[test]
    fn skin_model_round_trips_and_rejects_unknown_values() {
        assert_eq!(SkinModel::from_key("default"), Some(SkinModel::Default));
        assert_eq!(SkinModel::from_key("SLIM"), Some(SkinModel::Slim));
        assert_eq!(SkinModel::from_key("wide"), None);
        assert_eq!(serde_json::to_value(SkinModel::Slim).unwrap(), "slim");
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
