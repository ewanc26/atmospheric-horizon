//! Owner-only persistence for the labeler's K-256 signing key.

use super::{LabelSigner, service::LabelerError};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

#[derive(Debug, Serialize, Deserialize)]
struct KeyFile {
    #[serde(rename = "privateKey")]
    private_key: String,
    #[serde(rename = "publicKey")]
    public_key: String,
}

/// Load the key at `path`, or generate and save a new one. A malformed or
/// mismatched existing key is an error rather than a silent replacement.
pub(super) fn load_or_create(path: &Path) -> Result<LabelSigner, LabelerError> {
    if path.exists() {
        return load(path);
    }
    let signer = LabelSigner::generate();
    write(path, &signer)?;
    Ok(signer)
}

fn load(path: &Path) -> Result<LabelSigner, LabelerError> {
    let bytes = fs::read(path)?;
    let file: KeyFile = serde_json::from_slice(&bytes)
        .map_err(|error| LabelerError::Key(format!("invalid key file: {error}")))?;
    let secret: [u8; 32] = decode(&file.private_key)?
        .try_into()
        .map_err(|_| LabelerError::Key("privateKey must be 32 bytes".into()))?;
    let signer = LabelSigner::from_secret(secret)?;
    let public = decode(&file.public_key)?;
    if public != signer.public_key_bytes() {
        return Err(LabelerError::Key(
            "publicKey does not match privateKey".into(),
        ));
    }
    Ok(signer)
}

fn write(path: &Path, signer: &LabelSigner) -> Result<(), LabelerError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let file = KeyFile {
        private_key: STANDARD.encode(signer.secret_bytes()),
        public_key: STANDARD.encode(signer.public_key_bytes()),
    };
    let json = serde_json::to_vec_pretty(&file)?;
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
    Ok(())
}

fn decode(value: &str) -> Result<Vec<u8>, LabelerError> {
    let value = value.trim();
    STANDARD
        .decode(value)
        .or_else(|_| STANDARD_NO_PAD.decode(value.trim_end_matches('=')))
        .map_err(|error| LabelerError::Key(format!("invalid key encoding: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "atmospheric-label-key-{tag}-{}",
                std::process::id()
            ))
            .join("signing-key.json")
    }

    #[test]
    fn a_generated_key_is_reused_and_restricted() {
        let path = temp_path("reuse");
        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();
        assert_eq!(first.did_key(), second.did_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
