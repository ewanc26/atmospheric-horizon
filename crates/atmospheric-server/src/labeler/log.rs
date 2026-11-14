//! Append-only JSONL label log.
//!
//! Each line is `{seq, label}` with the spec label JSON shape. Writes are
//! flushed before the sequence number is reused, so a crash can lose at most
//! the last line; malformed lines are skipped when reopening.

use super::{label::SignedLabel, service::LabelerError};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub label: SignedLabel,
}

#[derive(Debug, Clone)]
pub struct LabelLog {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    next: Mutex<u64>,
}

impl LabelLog {
    /// Open the log and return it with its existing entries in sequence order.
    pub(super) fn open(path: PathBuf) -> Result<(Self, Vec<LogEntry>), LabelerError> {
        let mut entries = Vec::new();
        match fs::read_to_string(&path) {
            Ok(contents) => {
                for line in contents.lines().filter(|line| !line.trim().is_empty()) {
                    match serde_json::from_str::<LogEntry>(line) {
                        Ok(entry) => entries.push(entry),
                        Err(error) => {
                            eprintln!(
                                "atmospheric-server: skipping a malformed label log line: {error}"
                            );
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        entries.sort_by_key(|entry| entry.seq);
        let next = entries
            .last()
            .map(|entry| entry.seq.saturating_add(1))
            .unwrap_or(1);
        Ok((
            Self {
                inner: Arc::new(Inner {
                    path,
                    next: Mutex::new(next),
                }),
            },
            entries,
        ))
    }

    /// Append a signed label and return its assigned sequence number. The
    /// sequence is advanced only after the line is durable.
    pub(super) async fn append(&self, label: SignedLabel) -> Result<LogEntry, LabelerError> {
        let mut next = self.inner.next.lock().await;
        let seq = *next;
        let entry = LogEntry { seq, label };
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        let path = self.inner.path.clone();
        tokio::task::spawn_blocking(move || append_file(&path, &line)).await??;
        *next = seq.saturating_add(1);
        Ok(entry)
    }
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
    use crate::labeler::Label;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "atmospheric-label-log-{tag}-{}",
                std::process::id()
            ))
            .join("labels.jsonl")
    }

    fn label(val: &str) -> SignedLabel {
        SignedLabel::new(
            Label {
                ver: 1,
                src: "did:plc:labeler".into(),
                uri: "did:plc:alice".into(),
                cid: None,
                val: val.into(),
                neg: false,
                cts: "2026-11-14T00:00:00.000Z".into(),
                exp: None,
            },
            [0u8; 64],
        )
    }

    #[tokio::test]
    async fn entries_survive_reopen_and_sequence_continues() {
        let path = temp_path("reopen");
        let (log, entries) = LabelLog::open(path.clone()).unwrap();
        assert!(entries.is_empty());
        assert_eq!(log.append(label("online")).await.unwrap().seq, 1);
        assert_eq!(log.append(label("player")).await.unwrap().seq, 2);

        let (reopened, entries) = LabelLog::open(path.clone()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].seq, 2);
        assert_eq!(entries[1].label.label.val, "player");
        assert_eq!(reopened.append(label("admin")).await.unwrap().seq, 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
