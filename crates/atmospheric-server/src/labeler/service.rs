//! The labeler service: key ownership, label emission and query serving.

use super::{
    LabelError, LabelSigner, SignedLabel, key,
    label::Label,
    log::{LabelLog, LogEntry},
    query::Query,
    state::ActiveLabels,
    time,
};
use crate::model::{normalize_did, unix_now};
use serde::Serialize;
use std::{fmt, fs, path::Path, sync::Arc};
use tokio::sync::{Mutex, RwLock};

/// A request to sign and append one label. `src` is always the configured
/// labeler service DID; only the subject and value come from the caller.
#[derive(Debug, Clone)]
pub struct EmitLabel {
    pub uri: String,
    pub val: String,
    pub neg: bool,
    pub cid: Option<String>,
    pub exp: Option<String>,
}

/// A page of active labels returned by `queryLabels`.
#[derive(Debug, Serialize)]
pub struct QueryResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub labels: Vec<SignedLabel>,
}

#[derive(Debug, thiserror::Error)]
pub enum LabelerError {
    #[error("{0}")]
    Invalid(String),
    #[error("label signing key: {0}")]
    Key(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Sign(#[from] LabelError),
}

/// A cheap-to-clone handle to the labeler's signing key, durable log and
/// in-memory active state.
#[derive(Clone)]
pub struct Labeler {
    did: Arc<str>,
    signer: LabelSigner,
    log: LabelLog,
    active: Arc<RwLock<ActiveLabels>>,
    emit_lock: Arc<Mutex<()>>,
}

impl Labeler {
    /// Open the labeler state under `directory`, loading or creating its
    /// owner-only signing key and replaying the label log.
    pub fn open(directory: impl AsRef<Path>, did: &str) -> Result<Self, LabelerError> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory)?;
        let did = normalize_did(did).map_err(|error| LabelerError::Invalid(error.to_string()))?;
        let signer = key::load_or_create(&directory.join("signing-key.json"))?;
        let (log, entries) = LabelLog::open(directory.join("labels.jsonl"))?;
        let active = ActiveLabels::rebuild(&entries, unix_now());
        Ok(Self {
            did: Arc::from(did),
            signer,
            log,
            active: Arc::new(RwLock::new(active)),
            emit_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn did(&self) -> &str {
        &self.did
    }

    pub fn did_key(&self) -> String {
        self.signer.did_key()
    }

    /// Sign and append a label, then apply it to the active state. The
    /// sequence number is assigned by the durable log.
    pub async fn emit(&self, input: EmitLabel, now: u64) -> Result<LogEntry, LabelerError> {
        let uri = input.uri.trim().to_owned();
        if !valid_uri(&uri) {
            return Err(LabelerError::Invalid(
                "uri must be a DID or AT-URI without whitespace".into(),
            ));
        }
        let val = input.val.trim().to_owned();
        if val.is_empty()
            || val.len() > 128
            || val.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(LabelerError::Invalid(
                "val must be 1-128 non-whitespace characters".into(),
            ));
        }
        let cid = input
            .cid
            .map(|cid| cid.trim().to_owned())
            .filter(|cid| !cid.is_empty());
        if cid
            .as_deref()
            .is_some_and(|cid| cid.len() > 256 || cid.chars().any(char::is_whitespace))
        {
            return Err(LabelerError::Invalid(
                "cid must be a non-empty string without whitespace".into(),
            ));
        }
        let exp = input
            .exp
            .map(|exp| exp.trim().to_owned())
            .filter(|exp| !exp.is_empty());
        if exp
            .as_deref()
            .is_some_and(|exp| time::parse_rfc3339(exp).is_none())
        {
            return Err(LabelerError::Invalid(
                "exp must be an RFC 3339 timestamp".into(),
            ));
        }

        let _guard = self.emit_lock.lock().await;
        let label = Label {
            ver: 1,
            src: self.did.to_string(),
            uri,
            cid,
            val,
            neg: input.neg,
            cts: time::format_rfc3339(now),
            exp,
        };
        let signed = SignedLabel::new(label.clone(), self.signer.sign(&label));
        let entry = self.log.append(signed).await?;
        self.active.write().await.apply(&entry, now);
        Ok(entry)
    }

    /// Serve one `queryLabels` page from the active state.
    pub async fn query(&self, query: &Query, now: u64) -> QueryResult {
        let entries = self.active.read().await.query(
            &query.patterns,
            &query.sources,
            query.limit,
            query.cursor,
            now,
        );
        let cursor = if entries.len() == query.limit {
            entries.last().map(|entry| entry.seq.to_string())
        } else {
            None
        };
        QueryResult {
            cursor,
            labels: entries.into_iter().map(|entry| entry.label).collect(),
        }
    }
}

impl fmt::Debug for Labeler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Labeler")
            .field("did", &self.did)
            .field("signing_key", &self.signer.did_key())
            .finish_non_exhaustive()
    }
}

fn valid_uri(uri: &str) -> bool {
    (uri.starts_with("did:") || uri.starts_with("at://"))
        && uri.len() <= 2_048
        && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("atmospheric-labeler-{tag}-{}", std::process::id()))
    }

    fn emit(uri: &str, val: &str) -> EmitLabel {
        EmitLabel {
            uri: uri.into(),
            val: val.into(),
            neg: false,
            cid: None,
            exp: None,
        }
    }

    fn query(pattern: &str) -> Query {
        Query {
            patterns: vec![pattern.into()],
            sources: Vec::new(),
            limit: 50,
            cursor: 0,
        }
    }

    #[tokio::test]
    async fn labels_persist_and_queries_use_the_active_state() {
        let directory = temp_dir("persist");
        let labeler = Labeler::open(&directory, "did:plc:labeler").unwrap();
        let key = labeler.did_key();
        let first = labeler
            .emit(emit("did:plc:alice", "online"), 100)
            .await
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.label.label.cts, "1970-01-01T00:01:40.000Z");
        assert_eq!(first.label.label.src, "did:plc:labeler");

        let result = labeler.query(&query("did:plc:alice"), 100).await;
        assert_eq!(result.labels.len(), 1);
        assert!(result.cursor.is_none());

        let mut negation = emit("did:plc:alice", "online");
        negation.neg = true;
        labeler.emit(negation, 101).await.unwrap();
        assert!(
            labeler
                .query(&query("did:plc:alice"), 101)
                .await
                .labels
                .is_empty()
        );

        drop(labeler);
        let reopened = Labeler::open(&directory, "did:plc:labeler").unwrap();
        assert_eq!(reopened.did_key(), key);
        assert!(
            reopened
                .query(&query("did:plc:alice"), 101)
                .await
                .labels
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn expired_labels_are_not_served_and_input_is_validated() {
        let directory = temp_dir("expiry");
        let labeler = Labeler::open(&directory, "did:plc:labeler").unwrap();
        let mut expired = emit("did:plc:alice", "online");
        expired.exp = Some("1970-01-01T00:00:01Z".into());
        labeler.emit(expired, 100).await.unwrap();
        assert!(
            labeler
                .query(&query("did:plc:alice"), 100)
                .await
                .labels
                .is_empty()
        );

        assert!(
            labeler
                .emit(emit("not-a-uri", "online"), 100)
                .await
                .is_err()
        );
        assert!(
            labeler
                .emit(emit("did:plc:alice", "two words"), 100)
                .await
                .is_err()
        );
        let mut bad_exp = emit("did:plc:alice", "online");
        bad_exp.exp = Some("tomorrow".into());
        assert!(labeler.emit(bad_exp, 100).await.is_err());
        let _ = std::fs::remove_dir_all(directory);
    }
}
