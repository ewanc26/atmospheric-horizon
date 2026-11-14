//! Reading other players' `at.atpcraft.player.chat` records — the read side
//! of the atpcraft chat mirror that M5 opened on the write side. A player's
//! chat records live in *their own* repo, keyed by TID (time-ordered,
//! append-only), so the read is: resolve the account and its PDS (as the
//! M13 presence read does), page `com.atproto.repo.listRecords` for the chat
//! collection unauthenticated, parse each record, and order by `sentAt`.
//!
//! Chat records are NOT server-endorsed — the lexicon has no endorsement
//! fields, they are plain records in the author's own repo — so there is no
//! signature to verify; the digest says so. Wire shapes come from the
//! lexicons only: no code is copied from atpcraft (its repo carries no
//! licence).

use crate::records::{self, ChatRecord};

/// How many of the newest records the HUD shows.
pub const HISTORY_LIMIT: usize = 5;

/// The `message` truncation for the HUD line.
pub const MESSAGE_SNIPPET_LEN: usize = 80;

/// One chat record, reduced to what the HUD shows (M16).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatDigest {
    pub rkey: String,
    pub at_uri: String,
    pub sent_at: String,
    pub server: String,
    /// The message, truncated to [`MESSAGE_SNIPPET_LEN`] chars.
    pub message: String,
}

/// Parse one raw record value as a chat record (the `sentAt`/`message`/
/// `server` fields, `$type` ignored). `None` when malformed.
fn parse_chat(value: &serde_json::Value) -> Option<(ChatRecord, String)> {
    let record: ChatRecord = serde_json::from_value(value.clone()).ok()?;
    let sent_at = value["sentAt"].as_str()?.to_string();
    Some((record, sent_at))
}

/// Build the HUD digest of one chat record.
fn digest(did: &str, rkey: &str, record: &ChatRecord, sent_at: &str) -> ChatDigest {
    let message: String = record.message.chars().take(MESSAGE_SNIPPET_LEN).collect();
    let message = if record.message.chars().count() > MESSAGE_SNIPPET_LEN {
        format!("{message}…")
    } else {
        message
    };
    ChatDigest {
        rkey: rkey.into(),
        at_uri: format!("at://{did}/{}/{rkey}", records::CHAT_COLLECTION),
        sent_at: sent_at.to_string(),
        server: record.server.clone(),
        message,
    }
}

/// Reduce the listed records to (total parseable count, the newest
/// `limit` digests, oldest first), binding the subject DID into the AT-URIs.
/// Ordering is by `sentAt` (ISO 8601 sorts lexicographically; the TID rkeys
/// order the same way).
pub fn history(
    did: &str,
    records: &[(String, serde_json::Value)],
    limit: usize,
) -> (usize, Vec<ChatDigest>) {
    let mut parsed: Vec<(String, ChatRecord, String)> = records
        .iter()
        .filter_map(|(rkey, value)| {
            parse_chat(value).map(|(record, sent_at)| (rkey.clone(), record, sent_at))
        })
        .collect();
    parsed.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    let total = parsed.len();
    let start = total.saturating_sub(limit);
    let items = parsed
        .into_iter()
        .skip(start)
        .map(|(rkey, record, sent_at)| digest(did, &rkey, &record, &sent_at))
        .collect();
    (total, items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_value(sent_at: &str, message: &str, server: &str) -> serde_json::Value {
        serde_json::json!({
            "$type": records::CHAT_COLLECTION,
            "sentAt": sent_at,
            "message": message,
            "server": server,
        })
    }

    #[test]
    fn history_orders_by_sent_at_and_keeps_the_newest() {
        let records = vec![
            (
                "3jub4stub0001".to_string(),
                record_value("2026-11-12T10:00:00.000Z", "first", "srv"),
            ),
            (
                "3jub4stub0003".to_string(),
                record_value("2026-11-12T12:00:00.000Z", "third", "srv"),
            ),
            ("garbage".to_string(), serde_json::json!({"nope": true})),
            (
                "3jub4stub0002".to_string(),
                record_value("2026-11-12T11:00:00.000Z", "second", "other"),
            ),
        ];
        let (total, items) = history("did:plc:alice", &records, 2);
        assert_eq!(total, 3, "the garbage record is skipped");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].message, "second");
        assert_eq!(items[1].message, "third");
        assert_eq!(items[0].sent_at, "2026-11-12T11:00:00.000Z");
        assert_eq!(items[0].server, "other");
        assert_eq!(
            items[1].at_uri,
            "at://did:plc:alice/at.atpcraft.player.chat/3jub4stub0003"
        );
        assert_eq!(items[0].rkey, "3jub4stub0002");
        // Empty input.
        let (total, items) = history("did:plc:alice", &[], 5);
        assert_eq!(total, 0);
        assert!(items.is_empty());
    }

    #[test]
    fn history_truncates_long_messages_for_the_hud() {
        let long = "x".repeat(MESSAGE_SNIPPET_LEN + 50);
        let records = vec![(
            "3jub4stub0001".to_string(),
            record_value("2026-11-12T10:00:00.000Z", &long, "srv"),
        )];
        let (_, items) = history("did:plc:alice", &records, 5);
        assert_eq!(items[0].message.chars().count(), MESSAGE_SNIPPET_LEN + 1); // + the ellipsis
        assert!(items[0].message.ends_with('…'));
        // A short message stays as-is.
        let records = vec![(
            "3jub4stub0001".to_string(),
            record_value("2026-11-12T10:00:00.000Z", "hi", "srv"),
        )];
        let (_, items) = history("did:plc:alice", &records, 5);
        assert_eq!(items[0].message, "hi");
    }

    #[test]
    fn malformed_records_are_skipped_not_fatal() {
        let records = vec![
            ("k1".to_string(), serde_json::json!({"sentAt": 123})), // not a datetime string shape
            (
                "k2".to_string(),
                record_value("2026-11-12T10:00:00.000Z", "ok", "srv"),
            ),
        ];
        let (total, items) = history("did:plc:alice", &records, 5);
        assert_eq!(total, 1);
        assert_eq!(items[0].message, "ok");
    }
}
