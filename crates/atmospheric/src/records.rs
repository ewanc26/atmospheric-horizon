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

#[cfg(test)]
mod tests {
    use super::*;

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
}
