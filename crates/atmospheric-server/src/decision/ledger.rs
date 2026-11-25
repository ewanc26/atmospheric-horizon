//! Durable, signed moderation ledger for the sidecar (#199).
//!
//! Every record is signed with an owner-only Ed25519 key before it is appended
//! to `ledger/decisions.jsonl`. Public subjects are derived with a separate
//! owner-only pepper, so walking the JSONL or a published collection cannot
//! reverse a subject DID by dictionary attack. The in-memory vector is a read
//! mirror only: a failed append never changes what the API reports.

use crate::error::ApiError;
use atmospheric::{
    decision::{
        record::{
            DECISION_COLLECTION, DecisionEntry, DecisionKind, DecisionRecord, DecisionState,
            ResolvedDecision, resolve,
        },
        subject::{fresh_pepper, identifier},
    },
    signing::{self, SigningKey, SigningKeyFile},
    tid::TidGen,
};
use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, RwLock};

/// Upper bound on a public ledger page.
pub const MAX_LIMIT: usize = 200;

/// Startup policy for the ledger. The issuer DID is the server account whose
/// repository will hold the published records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionSettings {
    pub issuer: String,
}

impl DecisionSettings {
    pub fn new(issuer: &str) -> Result<Self, String> {
        let issuer = issuer.trim();
        if !(issuer.starts_with("did:plc:") || issuer.starts_with("did:web:")) {
            return Err("ledger issuer must be a did:plc or did:web identifier".into());
        }
        if issuer.len() > 256 || issuer.chars().any(char::is_whitespace) {
            return Err("ledger issuer is malformed".into());
        }
        Ok(Self {
            issuer: issuer.to_owned(),
        })
    }
}

/// Runtime status of an enabled ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerStatus {
    pub issuer: String,
    pub key_id: String,
    pub records: usize,
}

/// One submitted decision, before the server assigns its rkey and signature.
#[derive(Clone, Debug, Deserialize)]
pub struct DecisionInput {
    pub kind: DecisionKind,
    /// Plaintext subject DID for a new decision. Never stored or published.
    pub subject: Option<String>,
    pub category: String,
    pub scope: String,
    /// rkey of the decision a reversal or expiry ends.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub appeal: Option<String>,
}

/// Shared ledger handle. Cloning is cheap; writes are serialised.
#[derive(Clone)]
pub struct DecisionLedger {
    inner: Arc<LedgerInner>,
}

impl std::fmt::Debug for DecisionLedger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecisionLedger")
            .field("issuer", &self.inner.issuer)
            .field("key_id", &self.inner.key_id)
            .finish_non_exhaustive()
    }
}

struct LedgerInner {
    dir: PathBuf,
    issuer: String,
    signer: SigningKey,
    key_id: String,
    pepper: [u8; 32],
    entries: RwLock<Vec<DecisionEntry>>,
    write: Mutex<()>,
    tids: std::sync::Mutex<TidGen>,
}

impl DecisionLedger {
    /// Open a ledger, creating its owner-only key and pepper on first use.
    /// Existing records are verified before they become visible to readers.
    pub fn open(data_dir: &Path, settings: &DecisionSettings) -> Result<Self, ApiError> {
        let dir = data_dir.join("ledger");
        fs::create_dir_all(&dir)?;
        let signer = load_or_create_key(&dir.join("signing-key.json"))?;
        let pepper = load_or_create_pepper(&dir.join("subject-pepper.json"))?;
        let key_id = signing::key_id_of(&signer.verifying_key());
        let entries = load_entries(&dir.join("decisions.jsonl"), &settings.issuer)?;
        Ok(Self {
            inner: Arc::new(LedgerInner {
                dir,
                issuer: settings.issuer.clone(),
                signer,
                key_id,
                pepper,
                entries: RwLock::new(entries),
                write: Mutex::new(()),
                tids: std::sync::Mutex::new(TidGen::new()),
            }),
        })
    }

    /// Public verification key for every record in this ledger.
    pub fn key_id(&self) -> &str {
        &self.inner.key_id
    }

    pub fn public_key_spki_b64(&self) -> String {
        signing::public_key_spki_b64(&self.inner.signer.verifying_key())
    }

    pub async fn status(&self) -> LedgerStatus {
        LedgerStatus {
            issuer: self.inner.issuer.clone(),
            key_id: self.inner.key_id.clone(),
            records: self.inner.entries.read().await.len(),
        }
    }

    /// Sign and append one record. Reversals and expiries inherit the subject
    /// identifier of the decision they end, so a reader can group by subject
    /// without ever seeing a plaintext DID.
    pub async fn submit(&self, input: DecisionInput) -> Result<DecisionEntry, ApiError> {
        let _guard = self.inner.write.lock().await;
        let entries = self.inner.entries.read().await;
        let (subject, category, scope, target, expires_at, appeal) = match input.kind {
            DecisionKind::Decision => {
                let did =
                    input.subject.as_deref().map(str::trim).ok_or_else(|| {
                        ApiError::Invalid("a decision needs a subject DID".into())
                    })?;
                if !(did.starts_with("did:plc:") || did.starts_with("did:web:")) {
                    return Err(ApiError::Invalid(
                        "decision subject must be a did:plc or did:web DID".into(),
                    ));
                }
                (
                    identifier(&self.inner.pepper, did),
                    input.category,
                    input.scope,
                    None,
                    input.expires_at,
                    input.appeal,
                )
            }
            DecisionKind::Reversal | DecisionKind::Expiry => {
                let target = input
                    .target
                    .as_deref()
                    .map(str::trim)
                    .ok_or_else(|| ApiError::Invalid("an ending needs a target rkey".into()))?
                    .to_owned();
                let existing = entries
                    .iter()
                    .find(|entry| {
                        entry.rkey == target && entry.record.kind == DecisionKind::Decision
                    })
                    .ok_or_else(|| ApiError::NotFound(format!("no decision with rkey {target}")))?;
                (
                    existing.record.subject.clone(),
                    input.category,
                    input.scope,
                    Some(target),
                    None,
                    None,
                )
            }
        };
        drop(entries);

        let mut record = DecisionRecord {
            collection: DECISION_COLLECTION.into(),
            kind: input.kind,
            subject,
            category,
            scope,
            target,
            expires_at,
            appeal,
            issuer: self.inner.issuer.clone(),
            created_at: now_rfc3339(),
            signing_key: String::new(),
            signature: String::new(),
        };
        record.validate_structure().map_err(ApiError::Invalid)?;
        record.sign(&self.inner.signer);
        let rkey = {
            let mut tids = self
                .inner
                .tids
                .lock()
                .expect("tid generator is not poisoned");
            tids.next_tid()
        };
        let entry = DecisionEntry { rkey, record };
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        let path = self.inner.dir.join("decisions.jsonl");
        tokio::task::spawn_blocking(move || append_line(&path, &line)).await??;
        self.inner.entries.write().await.push(entry.clone());
        Ok(entry)
    }

    /// One raw record by repository key.
    pub async fn entry(&self, rkey: &str) -> Option<DecisionEntry> {
        self.inner
            .entries
            .read()
            .await
            .iter()
            .find(|entry| entry.rkey == rkey)
            .cloned()
    }

    /// Current state of every decision, newest first. `subject` and `state`
    /// narrow the result before the limit is applied.
    pub async fn resolved(
        &self,
        subject: Option<&str>,
        state: Option<DecisionState>,
        limit: usize,
    ) -> Vec<ResolvedDecision> {
        let entries = self.inner.entries.read().await;
        let mut resolved = resolve(&entries, crate::model::unix_now() as i64);
        if let Some(subject) = subject {
            resolved.retain(|item| item.record.subject == subject);
        }
        if let Some(state) = state {
            resolved.retain(|item| item.state == state);
        }
        resolved.truncate(limit.min(MAX_LIMIT));
        resolved
    }
}

/// The record timestamp in the API's RFC 3339 UTC form.
pub fn now_rfc3339() -> String {
    atrium_api::types::string::Datetime::now()
        .as_str()
        .to_owned()
}

fn load_or_create_key(path: &Path) -> Result<SigningKey, ApiError> {
    if path.exists() {
        let bytes = fs::read(path)?;
        let file: SigningKeyFile = serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::Corrupt(format!("ledger signing key: {error}")))?;
        return signing::from_key_file(&file)
            .map_err(|error| ApiError::Corrupt(format!("ledger signing key: {error}")));
    }
    let signer = signing::generate();
    let file = signing::key_file_of(&signer);
    write_secret(path, &serde_json::to_vec_pretty(&file)?)?;
    Ok(signer)
}

fn load_or_create_pepper(path: &Path) -> Result<[u8; 32], ApiError> {
    if path.exists() {
        let bytes = fs::read(path)?;
        let file: PepperFile = serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::Corrupt(format!("ledger subject pepper: {error}")))?;
        if file.version != 1 {
            return Err(ApiError::Corrupt("unknown ledger pepper version".into()));
        }
        let raw = general_purpose::STANDARD
            .decode(&file.pepper_b64)
            .map_err(|error| ApiError::Corrupt(format!("ledger subject pepper: {error}")))?;
        return raw
            .try_into()
            .map_err(|_| ApiError::Corrupt("ledger subject pepper has the wrong length".into()));
    }
    let pepper = fresh_pepper();
    let file = PepperFile {
        version: 1,
        pepper_b64: general_purpose::STANDARD.encode(pepper),
    };
    write_secret(path, &serde_json::to_vec_pretty(&file)?)?;
    Ok(pepper)
}

#[derive(Serialize, Deserialize)]
struct PepperFile {
    version: u32,
    pepper_b64: String,
}

fn load_entries(path: &Path, issuer: &str) -> Result<Vec<DecisionEntry>, ApiError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    let mut entries = Vec::new();
    let mut rkeys = std::collections::BTreeSet::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: DecisionEntry = serde_json::from_str(line)
            .map_err(|error| ApiError::Corrupt(format!("ledger line {}: {error}", index + 1)))?;
        if entry.record.verify().is_err() || entry.record.issuer != issuer {
            return Err(ApiError::Corrupt(format!(
                "ledger line {} failed signature or issuer verification",
                index + 1
            )));
        }
        if !rkeys.insert(entry.rkey.clone()) {
            return Err(ApiError::Corrupt(format!(
                "ledger line {} repeats rkey {}",
                index + 1,
                entry.rkey
            )));
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn write_secret(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        handle.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    handle.write_all(bytes)?;
    handle.sync_all()?;
    drop(handle);
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn append_line(path: &Path, line: &[u8]) -> Result<(), ApiError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(line)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DecisionSettings {
        DecisionSettings::new("did:plc:server").unwrap()
    }

    #[tokio::test]
    async fn ledger_persists_and_verifies_records() {
        let dir = std::env::temp_dir().join(format!(
            "atmospheric-ledger-test-{}-{}",
            std::process::id(),
            atmospheric::tid::TidGen::new().next_tid()
        ));
        let _ = fs::remove_dir_all(&dir);
        let ledger = DecisionLedger::open(&dir, &settings()).unwrap();
        let entry = ledger
            .submit(DecisionInput {
                kind: DecisionKind::Decision,
                subject: Some(" did:plc:alice ".into()),
                category: "spam".into(),
                scope: "server".into(),
                target: None,
                expires_at: None,
                appeal: None,
            })
            .await
            .unwrap();
        assert_eq!(entry.record.subject.len(), 64);
        assert!(
            !serde_json::to_string(&entry)
                .unwrap()
                .contains("did:plc:alice")
        );
        ledger
            .submit(DecisionInput {
                kind: DecisionKind::Reversal,
                subject: None,
                category: "appeal upheld".into(),
                scope: "server".into(),
                target: Some(entry.rkey.clone()),
                expires_at: None,
                appeal: None,
            })
            .await
            .unwrap();
        assert_eq!(ledger.status().await.records, 2);
        drop(ledger);

        let reopened = DecisionLedger::open(&dir, &settings()).unwrap();
        assert_eq!(reopened.status().await.records, 2);
        let resolved = reopened.resolved(None, None, MAX_LIMIT).await;
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].state, DecisionState::Reversed);
        assert_eq!(resolved[0].record.subject, entry.record.subject);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn issuer_policy_is_validated() {
        assert!(DecisionSettings::new("did:plc:server").is_ok());
        assert!(DecisionSettings::new("did:web:example.com").is_ok());
        assert!(DecisionSettings::new("@alice").is_err());
        assert!(DecisionSettings::new("did:plc:has space").is_err());
    }
}
