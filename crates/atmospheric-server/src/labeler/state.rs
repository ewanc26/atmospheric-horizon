//! Active (latest, non-negated) label state rebuilt from the label log.

use super::log::LogEntry;
use std::collections::BTreeMap;

/// The labels currently asserted per subject. A new assertion for the same
/// `(uri, val)` replaces the previous one; a negation removes it.
#[derive(Debug, Default)]
pub(super) struct ActiveLabels {
    subjects: BTreeMap<String, BTreeMap<String, LogEntry>>,
}

impl ActiveLabels {
    pub(super) fn rebuild(entries: &[LogEntry], now: u64) -> Self {
        let mut state = Self::default();
        for entry in entries {
            state.apply(entry, now);
        }
        state
    }

    pub(super) fn apply(&mut self, entry: &LogEntry, now: u64) {
        let label = &entry.label.label;
        if label.neg {
            if let Some(values) = self.subjects.get_mut(&label.uri) {
                values.remove(&label.val);
                if values.is_empty() {
                    self.subjects.remove(&label.uri);
                }
            }
            return;
        }
        if entry.label.is_expired(now) {
            return;
        }
        self.subjects
            .entry(label.uri.clone())
            .or_default()
            .insert(label.val.clone(), entry.clone());
    }

    pub(super) fn query(
        &self,
        patterns: &[String],
        sources: &[String],
        limit: usize,
        cursor: u64,
        now: u64,
    ) -> Vec<LogEntry> {
        let mut matches: Vec<LogEntry> = self
            .subjects
            .values()
            .flat_map(|values| values.values())
            .filter(|entry| {
                let label = &entry.label.label;
                !entry.label.is_expired(now)
                    && patterns
                        .iter()
                        .any(|pattern| matches_uri(&label.uri, pattern))
                    && (sources.is_empty() || sources.contains(&label.src))
                    && entry.seq > cursor
            })
            .cloned()
            .collect();
        matches.sort_by_key(|entry| entry.seq);
        matches.truncate(limit);
        matches
    }
}

fn matches_uri(uri: &str, pattern: &str) -> bool {
    match pattern {
        "*" => true,
        pattern if pattern.ends_with('*') => uri.starts_with(&pattern[..pattern.len() - 1]),
        pattern => uri == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labeler::{Label, SignedLabel};

    fn entry(seq: u64, uri: &str, val: &str, neg: bool, exp: Option<&str>) -> LogEntry {
        LogEntry {
            seq,
            label: SignedLabel::new(
                Label {
                    ver: 1,
                    src: "did:plc:labeler".into(),
                    uri: uri.into(),
                    cid: None,
                    val: val.into(),
                    neg,
                    cts: "2026-11-14T00:00:00.000Z".into(),
                    exp: exp.map(Into::into),
                },
                [0u8; 64],
            ),
        }
    }

    #[test]
    fn assertions_replace_negations_remove_and_expiry_is_filtered() {
        let mut state = ActiveLabels::default();
        state.apply(&entry(1, "did:plc:alice", "online", false, None), 100);
        state.apply(&entry(2, "did:plc:alice", "online", false, None), 100);
        state.apply(
            &entry(
                3,
                "did:plc:alice",
                "player",
                false,
                Some("1970-01-01T00:01:40Z"),
            ),
            100,
        );
        assert_eq!(
            state
                .query(&["did:plc:alice".into()], &[], 50, 0, 100)
                .len(),
            2
        );
        assert_eq!(
            state
                .query(&["did:plc:alice".into()], &[], 50, 0, 101)
                .len(),
            1
        );

        state.apply(&entry(4, "did:plc:alice", "online", true, None), 100);
        assert!(
            state
                .query(&["did:plc:alice".into()], &[], 50, 0, 101)
                .is_empty()
        );

        let rebuilt = ActiveLabels::rebuild(
            &[
                entry(1, "did:plc:alice", "online", false, None),
                entry(2, "did:plc:alice", "online", true, None),
            ],
            100,
        );
        assert!(rebuilt.query(&["*".into()], &[], 50, 0, 100).is_empty());
    }

    #[test]
    fn patterns_sources_and_cursors_filter_in_sequence_order() {
        let state = ActiveLabels::rebuild(
            &[
                entry(1, "did:plc:alice", "online", false, None),
                entry(2, "did:plc:bob", "online", false, None),
                entry(
                    3,
                    "at://did:plc:alice/app.bsky.feed.post/1",
                    "spam",
                    false,
                    None,
                ),
                entry(4, "did:plc:carol", "admin", false, None),
            ],
            100,
        );

        let all = state.query(&["*".into()], &[], 50, 0, 100);
        assert_eq!(
            all.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );

        let prefix = state.query(&["at://did:plc:alice/*".into()], &[], 50, 0, 100);
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0].seq, 3);

        let source_miss = state.query(&["*".into()], &["did:plc:other".into()], 50, 0, 100);
        assert!(source_miss.is_empty());
        let source_hit = state.query(&["*".into()], &["did:plc:labeler".into()], 50, 0, 100);
        assert_eq!(source_hit.len(), 4);

        let page = state.query(&["*".into()], &[], 2, 1, 100);
        assert_eq!(
            page.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [2, 3]
        );
        let after = state.query(&["*".into()], &[], 50, 2, 100);
        assert_eq!(
            after.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [3, 4]
        );
    }
}
