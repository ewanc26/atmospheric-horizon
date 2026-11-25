//! Wire and value types for Continuity Watchman (#190).
//!
//! Identity is keyed by DID only. Handles and PDS endpoints published in PLC
//! operations are observations, not credentials: they are reported with the
//! sequence that supplied them and are re-verified by the watcher before being
//! promoted to `verified_handles`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// How often the jittered resolution poll runs by default.
pub const DEFAULT_POLL_SECS: u64 = 300;
/// Spread added to each poll so a fleet of sidecars does not stampede PLC.
pub const DEFAULT_JITTER_SECS: u64 = 30;
pub const MIN_POLL_SECS: u64 = 30;
pub const MAX_POLL_SECS: u64 = 3600;
pub const MAX_JITTER_SECS: u64 = 300;
/// Default cap on retained identity events per process.
pub const DEFAULT_MAX_EVENTS: usize = 256;
/// Page size used for HTTP export catch-up when the stream cursor is stale.
pub const EXPORT_PAGE_SIZE: u16 = 1000;

/// One exported PLC operation from `/export` or `/export/stream`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOperation {
    pub seq: u64,
    pub did: String,
    pub operation: serde_json::Value,
    pub cid: String,
    pub created_at: String,
}

/// A DID-keyed identity observation. `verified_handles` is deliberately
/// separate from the handles claimed by a PLC operation or DID document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySnapshot {
    pub did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pds: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_handles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotation_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub verification_methods: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub observed_at: u64,
}

/// The identity fields worth reporting to a game server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityChange {
    Observed,
    PdsMigration,
    HandleChange,
    KeyRotation,
    SigningKeyChange,
    Tombstone,
    Restored,
}

impl IdentityChange {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::PdsMigration => "pds_migration",
            Self::HandleChange => "handle_change",
            Self::KeyRotation => "key_rotation",
            Self::SigningKeyChange => "signing_key_change",
            Self::Tombstone => "tombstone",
            Self::Restored => "restored",
        }
    }
}

/// One recorded identity change, bound to the DID and optionally the linked
/// Minecraft UUID. The UUID is looked up from the existing identity map and is
/// never taken from a PLC claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityEvent {
    pub id: u64,
    pub did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub changes: Vec<IdentityChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub observed_at: u64,
    pub previous: Option<IdentitySnapshot>,
    pub current: IdentitySnapshot,
}

/// One DID on the watch list, with the linked UUID resolved from the existing
/// identity map rather than from any PLC claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchedIdentity {
    pub did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub has_snapshot: bool,
}

/// Bounded, in-memory watcher state exposed by the status route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuityStatus {
    pub watched: usize,
    pub snapshots: usize,
    pub events: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    pub poll_secs: u64,
    pub jitter_secs: u64,
    pub plc_directory: String,
}

/// Startup policy for the optional watcher. PLC is a trusted third party for
/// ordering, so operators may point both resolution and the stream at a mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuitySettings {
    pub plc_directory: String,
    pub poll_secs: u64,
    pub jitter_secs: u64,
}

impl Default for ContinuitySettings {
    fn default() -> Self {
        Self {
            plc_directory: "https://plc.directory".into(),
            poll_secs: DEFAULT_POLL_SECS,
            jitter_secs: DEFAULT_JITTER_SECS,
        }
    }
}

impl ContinuitySettings {
    /// Refuse malformed mirrors and poll windows that would stampede PLC.
    pub fn validate(&self) -> Result<(), String> {
        let base = self.plc_directory.trim();
        if !(base.starts_with("http://") || base.starts_with("https://"))
            || base.chars().any(char::is_whitespace)
        {
            return Err(format!(
                "invalid PLC directory URL: {:?}",
                self.plc_directory
            ));
        }
        if !(MIN_POLL_SECS..=MAX_POLL_SECS).contains(&self.poll_secs) {
            return Err(format!(
                "continuity poll interval must be {MIN_POLL_SECS}–{MAX_POLL_SECS} seconds"
            ));
        }
        if self.jitter_secs > MAX_JITTER_SECS || self.jitter_secs > self.poll_secs {
            return Err(format!(
                "continuity jitter must be at most {MAX_JITTER_SECS} seconds and no larger than the poll interval"
            ));
        }
        Ok(())
    }
}

/// Reject anything that is not a DID before it enters the watch list. A handle
/// here would let a squatter move the observed identity, which #190 forbids.
pub fn validate_watch_did(did: &str) -> Result<String, String> {
    let did = did.trim();
    if !did.starts_with("did:plc:") {
        return Err("continuity watches require a did:plc identifier, not a handle".into());
    }
    if did.chars().any(char::is_whitespace) || did.len() != 32 {
        return Err(format!("invalid did:plc identifier: {did:?}"));
    }
    Ok(did.to_owned())
}

/// Parse one newline-delimited `/export` record or WebSocket frame. Legacy
/// timestamp-format records have no sequence number and are ignored; the poll
/// still observes those accounts through DID resolution.
pub fn parse_export_line(line: &str) -> Result<Option<ExportOperation>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    if let Some(kind) = value["type"].as_str()
        && !matches!(kind, "sequenced_op" | "indexed_op")
    {
        return Ok(None);
    }
    if value.get("seq").is_none() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value(value)?))
}

/// WebSocket URL for the PLC export stream, derived from the configured mirror
/// so a replica needs no second setting.
pub fn export_stream_url(directory: &str, cursor: Option<u64>) -> Result<String, String> {
    let base = directory.trim().trim_end_matches('/');
    let scheme = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return Err(format!("invalid PLC directory URL: {directory:?}"));
    };
    Ok(match cursor {
        Some(cursor) => format!("{scheme}/export/stream?cursor={cursor}"),
        None => format!("{scheme}/export/stream"),
    })
}

/// HTTP URL for sequence-based export catch-up.
pub fn export_page_url(directory: &str, after: u64) -> Result<String, String> {
    let base = directory.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(format!("invalid PLC directory URL: {directory:?}"));
    }
    Ok(format!(
        "{base}/export?after={after}&count={EXPORT_PAGE_SIZE}"
    ))
}

/// Build a DID-keyed snapshot from a PLC operation or tombstone.
pub fn snapshot_from_operation(
    did: &str,
    operation: &serde_json::Value,
    seq: Option<u64>,
    cid: Option<String>,
    created_at: Option<String>,
    uuid: Option<String>,
    observed_at: u64,
) -> IdentitySnapshot {
    let mut snapshot = IdentitySnapshot {
        did: did.to_owned(),
        uuid,
        pds: None,
        handles: Vec::new(),
        verified_handles: Vec::new(),
        rotation_keys: Vec::new(),
        verification_methods: BTreeMap::new(),
        cid,
        seq,
        observed_at,
    };
    let _ = created_at; // carried on the event, not stored twice on the snapshot

    let kind = operation["type"].as_str().unwrap_or_default();
    if kind == "plc_tombstone" {
        return snapshot;
    }

    if kind == "create" {
        // Deprecated genesis format, still returned by PLC forever.
        if let Some(key) = operation["signingKey"].as_str() {
            snapshot
                .verification_methods
                .insert("atproto".into(), key.to_owned());
        }
        if let Some(key) = operation["recoveryKey"].as_str() {
            snapshot.rotation_keys.push(key.to_owned());
        }
        if let Some(handle) = operation["handle"]
            .as_str()
            .and_then(normalize_claimed_handle)
        {
            snapshot.handles.push(handle);
        }
        if let Some(endpoint) = operation["service"].as_str().and_then(valid_endpoint) {
            snapshot.pds = Some(endpoint.to_owned());
        }
    } else {
        snapshot.rotation_keys = string_array(operation.get("rotationKeys"));
        for handle in string_array(operation.get("alsoKnownAs")) {
            if let Some(handle) = normalize_claimed_handle(&handle) {
                snapshot.handles.push(handle);
            }
        }
        if let Some(methods) = operation["verificationMethods"].as_object() {
            for (name, value) in methods {
                if let Some(key) = value.as_str() {
                    snapshot
                        .verification_methods
                        .insert(name.clone(), key.to_owned());
                }
            }
        }
        snapshot.pds = operation
            .get("services")
            .and_then(pds_from_services)
            .map(str::to_owned);
    }

    dedup(&mut snapshot.handles);
    dedup(&mut snapshot.rotation_keys);
    snapshot
}

/// Build a snapshot from a resolved DID document. Rotation keys are not in the
/// document, so the watcher fills them from the PLC `/data` response.
pub fn snapshot_from_did_document(
    did: &str,
    document: &serde_json::Value,
    uuid: Option<String>,
    observed_at: u64,
) -> IdentitySnapshot {
    let mut snapshot = IdentitySnapshot {
        did: did.to_owned(),
        uuid,
        pds: document
            .get("service")
            .and_then(pds_from_did_document)
            .map(str::to_owned),
        handles: Vec::new(),
        verified_handles: Vec::new(),
        rotation_keys: Vec::new(),
        verification_methods: BTreeMap::new(),
        cid: None,
        seq: None,
        observed_at,
    };
    if let Some(names) = document["alsoKnownAs"].as_array() {
        for name in names {
            if let Some(name) = name.as_str().and_then(normalize_claimed_handle) {
                snapshot.handles.push(name);
            }
        }
    }
    if let Some(methods) = document["verificationMethod"].as_array() {
        for method in methods {
            let Some(id) = method["id"].as_str() else {
                continue;
            };
            let Some(key) = method["publicKeyMultibase"].as_str() else {
                continue;
            };
            let name = fragment(id).unwrap_or(id.trim()).to_owned();
            if !name.is_empty() {
                snapshot.verification_methods.insert(name, key.to_owned());
            }
        }
    }
    dedup(&mut snapshot.handles);
    snapshot
}

/// Compare only identity-bearing fields; sequence, CID and observation time
/// are provenance and do not themselves constitute a change.
pub fn diff_snapshots(
    previous: Option<&IdentitySnapshot>,
    current: &IdentitySnapshot,
) -> Vec<IdentityChange> {
    let Some(previous) = previous else {
        return vec![IdentityChange::Observed];
    };
    let mut changes = Vec::new();
    if previous.pds != current.pds {
        changes.push(if current.pds.is_none() {
            IdentityChange::Tombstone
        } else if previous.pds.is_none() {
            IdentityChange::Restored
        } else {
            IdentityChange::PdsMigration
        });
    }
    if previous.handles != current.handles {
        changes.push(IdentityChange::HandleChange);
    }
    if previous.rotation_keys != current.rotation_keys {
        changes.push(IdentityChange::KeyRotation);
    }
    if previous.verification_methods != current.verification_methods {
        changes.push(IdentityChange::SigningKeyChange);
    }
    changes
}

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn pds_from_services(services: &serde_json::Value) -> Option<&str> {
    let entry = services.as_object()?.get("atproto_pds")?;
    if entry["type"].as_str() != Some("AtprotoPersonalDataServer") {
        return None;
    }
    entry["endpoint"].as_str().and_then(valid_endpoint)
}

fn pds_from_did_document(services: &serde_json::Value) -> Option<&str> {
    services.as_array()?.iter().find_map(|entry| {
        if !fragment_matches(entry["id"].as_str(), "atproto_pds")
            || entry["type"].as_str() != Some("AtprotoPersonalDataServer")
        {
            return None;
        }
        entry["serviceEndpoint"].as_str().and_then(valid_endpoint)
    })
}

/// DID documents use either `#fragment` or the full `did#fragment` form. Both
/// name the same verification method or service, so index by fragment only.
fn fragment(id: &str) -> Option<&str> {
    let id = id.trim();
    id.strip_prefix('#')
        .or_else(|| id.rsplit_once('#').map(|(_, fragment)| fragment))
}

fn fragment_matches(id: Option<&str>, expected: &str) -> bool {
    id.and_then(fragment) == Some(expected)
}

fn valid_endpoint(endpoint: &str) -> Option<&str> {
    let endpoint = endpoint.trim();
    if (endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        && !endpoint.chars().any(char::is_whitespace)
    {
        Some(endpoint)
    } else {
        None
    }
}

fn normalize_claimed_handle(value: &str) -> Option<String> {
    let value = value.trim();
    let handle = value
        .strip_prefix("at://")
        .or_else(|| (!value.contains("://")).then_some(value))?;
    let handle = handle.trim_end_matches('/').trim().to_ascii_lowercase();
    if handle.is_empty() || handle.len() > 253 || handle.chars().any(char::is_whitespace) {
        return None;
    }
    Some(handle)
}

fn dedup(values: &mut Vec<String>) {
    let unique: BTreeSet<String> = values.iter().cloned().collect();
    *values = unique.into_iter().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_sequenced_operation_becomes_a_did_keyed_snapshot() {
        let operation = json!({
            "type": "plc_operation",
            "rotationKeys": ["did:key:zRecovery", "did:key:zRotation"],
            "alsoKnownAs": ["at://Alice.test"],
            "verificationMethods": {"atproto": "did:key:zSigning"},
            "services": {"atproto_pds": {
                "type": "AtprotoPersonalDataServer",
                "endpoint": "https://pds.example"
            }}
        });
        let snapshot = snapshot_from_operation(
            "did:plc:alice",
            &operation,
            Some(42),
            Some("bafy".into()),
            Some("2026-11-25T00:00:00.000Z".into()),
            Some("uuid-1".into()),
            100,
        );
        assert_eq!(snapshot.did, "did:plc:alice");
        assert_eq!(snapshot.uuid.as_deref(), Some("uuid-1"));
        assert_eq!(snapshot.pds.as_deref(), Some("https://pds.example"));
        assert_eq!(snapshot.handles, ["alice.test"]);
        assert_eq!(snapshot.rotation_keys.len(), 2);
        assert_eq!(snapshot.verification_methods["atproto"], "did:key:zSigning");
        assert_eq!(snapshot.seq, Some(42));
        assert!(snapshot.verified_handles.is_empty());
    }

    #[test]
    fn tombstone_and_legacy_operations_are_understood() {
        let tombstone = snapshot_from_operation(
            "did:plc:alice",
            &json!({"type": "plc_tombstone", "prev": "bafy"}),
            Some(9),
            None,
            None,
            None,
            1,
        );
        assert_eq!(tombstone.pds, None);
        assert!(tombstone.handles.is_empty());

        let legacy = snapshot_from_operation(
            "did:plc:alice",
            &json!({
                "type": "create",
                "signingKey": "did:key:zSigning",
                "recoveryKey": "did:key:zRecovery",
                "handle": "alice.test",
                "service": "https://old.example"
            }),
            Some(1),
            None,
            None,
            None,
            2,
        );
        assert_eq!(legacy.pds.as_deref(), Some("https://old.example"));
        assert_eq!(legacy.handles, ["alice.test"]);
        assert_eq!(legacy.rotation_keys, ["did:key:zRecovery"]);
        assert_eq!(legacy.verification_methods["atproto"], "did:key:zSigning");
    }

    #[test]
    fn did_documents_key_full_did_urls_by_fragment() {
        let did = "did:plc:abcdefghijklmnopqrstuvwx";
        let document = json!({
            "alsoKnownAs": ["at://alice.test"],
            "verificationMethod": [{
                "id": format!("{did}#atproto"),
                "publicKeyMultibase": "did:key:zSigning"
            }],
            "service": [{
                "id": format!("{did}#atproto_pds"),
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.example"
            }]
        });
        let snapshot = snapshot_from_did_document(did, &document, Some("uuid-1".into()), 7);
        assert_eq!(snapshot.pds.as_deref(), Some("https://pds.example"));
        assert_eq!(snapshot.handles, ["alice.test"]);
        assert_eq!(snapshot.verification_methods["atproto"], "did:key:zSigning");
        assert_eq!(snapshot.seq, None);
        assert_eq!(snapshot.observed_at, 7);
    }

    #[test]
    fn differences_are_named_for_the_game_server() {
        let first = snapshot_from_operation(
            "did:plc:alice",
            &json!({
                "type": "plc_operation",
                "rotationKeys": ["did:key:zOld"],
                "alsoKnownAs": ["at://alice.test"],
                "verificationMethods": {"atproto": "did:key:zSigning"},
                "services": {"atproto_pds": {
                    "type": "AtprotoPersonalDataServer", "endpoint": "https://old.example"
                }}
            }),
            Some(1),
            None,
            None,
            None,
            1,
        );
        let mut migrated = first.clone();
        migrated.pds = Some("https://new.example".into());
        migrated.rotation_keys = vec!["did:key:zNew".into()];
        assert_eq!(
            diff_snapshots(Some(&first), &migrated),
            [IdentityChange::PdsMigration, IdentityChange::KeyRotation]
        );

        let mut tombstoned = migrated.clone();
        tombstoned.pds = None;
        assert_eq!(
            diff_snapshots(Some(&migrated), &tombstoned),
            [IdentityChange::Tombstone]
        );
        assert_eq!(diff_snapshots(None, &first), [IdentityChange::Observed]);
    }

    #[test]
    fn export_lines_and_urls_are_validated() {
        let line = json!({
            "type": "sequenced_op",
            "seq": 7,
            "did": "did:plc:alice",
            "operation": {"type": "plc_operation"},
            "cid": "bafy",
            "createdAt": "2026-11-25T00:00:00.000Z"
        })
        .to_string();
        let parsed = parse_export_line(&line).unwrap().unwrap();
        assert_eq!(parsed.seq, 7);
        assert!(
            parse_export_line(r#"{"did":"did:plc:x"}"#)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            export_stream_url("https://plc.directory", Some(7)).unwrap(),
            "wss://plc.directory/export/stream?cursor=7"
        );
        assert_eq!(
            export_page_url("http://mirror.test/", 7).unwrap(),
            "http://mirror.test/export?after=7&count=1000"
        );
        assert!(export_stream_url("plc.directory", None).is_err());
    }

    #[test]
    fn watches_reject_handles_and_short_dids() {
        assert!(validate_watch_did(" did:plc:abcdefghijklmnopqrstuvwx ").is_ok());
        assert!(validate_watch_did("alice.test").is_err());
        assert!(validate_watch_did("did:plc:short").is_err());
    }

    #[test]
    fn settings_bound_the_poll_window() {
        let mut settings = ContinuitySettings::default();
        settings.validate().unwrap();
        settings.poll_secs = MIN_POLL_SECS - 1;
        assert!(settings.validate().is_err());
        settings.poll_secs = 60;
        settings.jitter_secs = 61;
        assert!(settings.validate().is_err());
        settings.jitter_secs = 30;
        settings.plc_directory = "plc.directory".into();
        assert!(settings.validate().is_err());
    }
}
