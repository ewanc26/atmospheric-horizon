use crate::{error::ApiError, model::Link};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub(super) const STORE_VERSION: u32 = 1;
pub(super) const PAIRING_FILE: &str = "links.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DiskState {
    #[serde(default = "store_version")]
    pub(super) version: u32,
    #[serde(default)]
    pub(super) links: BTreeMap<String, Link>,
    #[serde(default)]
    pub(super) pairings: BTreeMap<String, PendingPairing>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PendingPairing {
    pub(super) player_uuid: String,
    pub(super) player_name: String,
    pub(super) expires_at: u64,
}

impl DiskState {
    pub(super) fn empty() -> Self {
        Self {
            version: STORE_VERSION,
            links: BTreeMap::new(),
            pairings: BTreeMap::new(),
        }
    }
}

fn store_version() -> u32 {
    STORE_VERSION
}

pub(super) async fn persist(path: PathBuf, state: DiskState) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || persist_sync(&path, &state)).await??;
    Ok(())
}

fn persist_sync(path: &Path, state: &DiskState) -> Result<(), ApiError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(state)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    #[cfg(windows)]
    if path.exists() {
        // std::fs::rename does not replace an existing destination on Windows.
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

/// The default data file inside a data directory.
pub fn default_links_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join(PAIRING_FILE)
}
