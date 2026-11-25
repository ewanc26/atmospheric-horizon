//! Signed, append-only receipts for every grant-exercised admin operation.
//!
//! The key is generated on first use and persisted owner-only beside the
//! server's other local state. A receipt proves which DID used which scoped
//! grant for which operation, but never contains the grant token itself.

use super::grant::Grant;
use crate::error::ApiError;
use atmospheric::signing::{self, SigningKey, SigningKeyFile};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

/// One signed audit receipt. `signature` covers every other field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub seq: u64,
    pub issued_at: u64,
    pub did: String,
    pub scope: String,
    pub action: String,
    pub target: String,
    pub grant_id: String,
    pub outcome: String,
    pub key_id: String,
    pub signature: String,
}

#[derive(Serialize)]
struct ReceiptPayload<'a> {
    seq: u64,
    issued_at: u64,
    did: &'a str,
    scope: &'a str,
    action: &'a str,
    target: &'a str,
    grant_id: &'a str,
    outcome: &'a str,
    key_id: &'a str,
}

impl Receipt {
    fn payload_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&ReceiptPayload {
            seq: self.seq,
            issued_at: self.issued_at,
            did: &self.did,
            scope: &self.scope,
            action: &self.action,
            target: &self.target,
            grant_id: &self.grant_id,
            outcome: &self.outcome,
            key_id: &self.key_id,
        })
        .expect("receipt fields are serializable")
    }

    /// Verify the signature against the key named by this receipt. The caller
    /// must have obtained that key from the corresponding `/audit-key` endpoint
    /// or key file; name binding here prevents silent cross-key substitution.
    pub fn verify(&self, key_id: &str, public_key_spki_b64: &str) -> Result<(), String> {
        if self.key_id != key_id {
            return Err("receipt names a different signing key".into());
        }
        let key = signing::verifying_key_from_spki_b64(public_key_spki_b64)
            .map_err(|error| error.to_string())?;
        signing::verify_b64(&key, &self.payload_bytes(), &self.signature)
            .map_err(|error| error.to_string())
    }
}

/// Persisted audit log plus its signing key.
#[derive(Debug, Clone)]
pub struct AuditLog {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    signer: SigningKey,
    key_id: String,
    next: Mutex<u64>,
}

impl AuditLog {
    /// Open (or create) the audit log in `directory`.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, ApiError> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory)?;
        let signer = load_or_create_key(&directory.join("audit-key.json"))?;
        let key_id = signing::key_id_of(&signer.verifying_key());
        let path = directory.join("receipts.jsonl");
        let mut next = 1;
        match fs::read_to_string(&path) {
            Ok(contents) => {
                for line in contents.lines().filter(|line| !line.trim().is_empty()) {
                    match serde_json::from_str::<Receipt>(line) {
                        Ok(receipt) => {
                            next = next.max(receipt.seq.saturating_add(1));
                        }
                        Err(error) => {
                            eprintln!(
                                "atmospheric-server: skipping malformed audit receipt line: {error}"
                            );
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                signer,
                key_id,
                next: Mutex::new(next),
            }),
        })
    }

    /// Stable id of the key that signs receipts from this log.
    pub fn key_id(&self) -> &str {
        &self.inner.key_id
    }

    /// Public key for verifying receipts, as a base64 SPKI DER value.
    pub fn public_key_spki_b64(&self) -> String {
        signing::public_key_spki_b64(&self.inner.signer.verifying_key())
    }

    /// Return the receipts in sequence order.
    pub async fn receipts(&self) -> Result<Vec<Receipt>, ApiError> {
        let contents = match tokio::fs::read_to_string(&self.inner.path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut receipts = Vec::new();
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<Receipt>(line) {
                Ok(receipt) => receipts.push(receipt),
                Err(error) => {
                    eprintln!("atmospheric-server: skipping malformed audit receipt line: {error}");
                }
            }
        }
        receipts.sort_by_key(|receipt| receipt.seq);
        Ok(receipts)
    }

    /// Sign and durably append a receipt for one grant-exercised operation.
    pub async fn append(
        &self,
        grant: &Grant,
        scope: &str,
        action: &str,
        target: &str,
        outcome: &str,
    ) -> Result<Receipt, ApiError> {
        let mut next = self.inner.next.lock().await;
        let now = super::challenge::now_secs();
        let mut receipt = Receipt {
            seq: *next,
            issued_at: now,
            did: grant.did.clone(),
            scope: scope.to_owned(),
            action: action.to_owned(),
            target: target.to_owned(),
            grant_id: grant.id.clone(),
            outcome: outcome.to_owned(),
            key_id: self.inner.key_id.clone(),
            signature: String::new(),
        };
        receipt.signature = signing::sign_b64(&self.inner.signer, &receipt.payload_bytes());
        let mut line = serde_json::to_vec(&receipt)?;
        line.push(b'\n');
        let path = self.inner.path.clone();
        tokio::task::spawn_blocking(move || append_file(&path, &line)).await??;
        *next = receipt.seq.saturating_add(1);
        Ok(receipt)
    }
}

fn load_or_create_key(path: &Path) -> Result<SigningKey, ApiError> {
    if path.exists() {
        let bytes = fs::read(path)?;
        let file: SigningKeyFile = serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::Corrupt(format!("operator audit key: {error}")))?;
        return signing::from_key_file(&file)
            .map_err(|error| ApiError::Corrupt(format!("operator audit key: {error}")));
    }
    let signer = signing::generate();
    let file = signing::key_file_of(&signer);
    let json = serde_json::to_vec_pretty(&file)?;
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
    handle.write_all(&json)?;
    handle.sync_all()?;
    drop(handle);
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(signer)
}

fn append_file(path: &Path, line: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(line)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::{
        grant::Grants,
        model::{GrantScope, OperatorSettings},
    };

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "atmospheric-operator-audit-{tag}-{}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn receipts_are_signed_and_reloaded() {
        let dir = temp_dir("signed");
        let _ = fs::remove_dir_all(&dir);
        let log = AuditLog::open(&dir).unwrap();
        let (_, grant) = Grants::default()
            .issue(&OperatorSettings::default(), "did:plc:op", 1_000)
            .unwrap();
        log.append(&grant, "links_write", "put_link", "uuid", "ok")
            .await
            .unwrap();

        let reloaded = AuditLog::open(&dir).unwrap();
        assert_eq!(reloaded.key_id(), log.key_id());
        let receipts = reloaded.receipts().await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].seq, 1);
        assert_eq!(receipts[0].did, "did:plc:op");
        receipts[0]
            .verify(log.key_id(), &log.public_key_spki_b64())
            .unwrap();
        let mut tampered = receipts[0].clone();
        tampered.target = "other".into();
        assert!(
            tampered
                .verify(log.key_id(), &log.public_key_spki_b64())
                .is_err()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_file_is_owner_only() {
        let dir = temp_dir("perms");
        let _ = fs::remove_dir_all(&dir);
        let log = AuditLog::open(&dir).unwrap();
        let _ = GrantScope::LinksRead; // keeps the scope import meaningful
        assert!(!log.key_id().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("audit-key.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
