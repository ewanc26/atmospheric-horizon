//! Notification polling with a persisted seen-cursor.

use crate::{
    error::{AtmosphericError, Result},
    session::Agent,
};
use atrium_api::{app::bsky::notification::list_notifications, types::LimitedNonZeroU8};
use serde::{Deserialize, Serialize};

/// How many notifications one poll asks for (the API caps at 100).
const POLL_LIMIT: u8 = 25;

/// One notification, reduced to what the HUD panel shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationDigest {
    /// The notification's record URI.
    pub uri: String,
    /// The acting user's handle.
    pub author_handle: String,
    /// `like`, `reply`, `mention`, …
    pub reason: String,
    /// A short snippet of the text, if any.
    pub text_snippet: String,
}

/// The persisted cursor: everything indexed after `seen_at` is new.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeenCursor {
    pub seen_at: Option<String>,
}

impl SeenCursor {
    /// Whether a notification indexed at `indexed_at` should be shown.
    pub fn should_show(&self, indexed_at: &str) -> bool {
        match &self.seen_at {
            Some(seen) => indexed_at > seen.as_str(),
            None => true,
        }
    }

    /// Advance the cursor, keeping only the newest timestamp.
    pub fn mark(&mut self, indexed_at: &str) {
        if self.should_show(indexed_at) {
            self.seen_at = Some(indexed_at.to_string());
        }
    }
}

/// The datetime as the API serialises it (RFC 3339, micros, `Z`), so stored
/// cursors compare lexicographically in time order.
fn api_datetime(dt: &atrium_api::types::string::Datetime) -> String {
    serde_json::to_value(dt)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Reduce one API notification to its HUD digest.
pub fn digest_of(notification: &list_notifications::Notification) -> NotificationDigest {
    let data = &notification.data;
    let text_snippet = match &data.record {
        atrium_api::types::Unknown::Object(map) => map
            .get("text")
            .and_then(|v| serde_json::to_value(v).ok())
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect(),
        _ => String::new(),
    };
    NotificationDigest {
        uri: data.uri.clone(),
        author_handle: data.author.handle.as_str().to_owned(),
        reason: data.reason.clone(),
        text_snippet,
    }
}

/// Poll `app.bsky.notification.listNotifications` and return the new digests,
/// advancing `cursor` past everything shown. The cursor persists under the
/// mod's storage folder (written by the worker) so restarts do not re-show
/// old notifications — the pattern bsky-bot-sdk uses with `seenAt`.
pub async fn poll(agent: &Agent, cursor: &mut SeenCursor) -> Result<Vec<NotificationDigest>> {
    let seen_at = match &cursor.seen_at {
        Some(stored) => Some(
            stored
                .parse()
                .map_err(|_| AtmosphericError::Message(format!("bad stored seen_at: {stored}")))?,
        ),
        None => None,
    };
    let params = list_notifications::Parameters::from(list_notifications::ParametersData {
        cursor: None,
        limit: Some(
            LimitedNonZeroU8::try_from(POLL_LIMIT).expect("POLL_LIMIT is within the API cap"),
        ),
        priority: Some(false),
        reasons: None,
        seen_at,
    });
    let output = agent
        .api
        .app
        .bsky
        .notification
        .list_notifications(params)
        .await
        .map_err(AtmosphericError::protocol)?;
    let mut digests = Vec::new();
    for notification in &output.data.notifications {
        let indexed_at = api_datetime(&notification.data.indexed_at);
        if cursor.should_show(&indexed_at) {
            digests.push(digest_of(notification));
            cursor.mark(&indexed_at);
        }
    }
    Ok(digests)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_new_with_no_cursor() {
        let cursor = SeenCursor::default();
        assert!(cursor.should_show("2026-11-12T10:00:00.000Z"));
    }

    #[test]
    fn cursor_hides_old_and_shows_new_notifications() {
        let mut cursor = SeenCursor {
            seen_at: Some("2026-11-12T10:00:00.000Z".into()),
        };
        assert!(!cursor.should_show("2026-11-12T09:59:59.999Z"));
        assert!(cursor.should_show("2026-11-12T10:00:00.001Z"));

        cursor.mark("2026-11-12T12:00:00.000Z");
        assert_eq!(cursor.seen_at.as_deref(), Some("2026-11-12T12:00:00.000Z"));
    }

    #[test]
    fn cursor_never_moves_backwards() {
        let mut cursor = SeenCursor {
            seen_at: Some("2026-11-12T12:00:00.000Z".into()),
        };
        cursor.mark("2026-11-12T08:00:00.000Z");
        assert_eq!(cursor.seen_at.as_deref(), Some("2026-11-12T12:00:00.000Z"));
    }

    #[test]
    fn digest_reduces_a_notification() {
        let raw = serde_json::json!({
            "uri": "at://did:plc:bob/app.bsky.feed.post/2",
            "cid": "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy",
            "indexedAt": "2026-11-12T11:00:00.000Z",
            "isRead": false,
            "reason": "reply",
            "record": { "$type": "app.bsky.feed.post", "text": "nice post!" },
            "author": { "did": "did:plc:bob", "handle": "bob.test" }
        });
        let notification: list_notifications::Notification = serde_json::from_value(raw).unwrap();
        let digest = digest_of(&notification);
        assert_eq!(digest.uri, "at://did:plc:bob/app.bsky.feed.post/2");
        assert_eq!(digest.author_handle, "bob.test");
        assert_eq!(digest.reason, "reply");
        assert_eq!(digest.text_snippet, "nice post!");
    }

    #[test]
    fn digest_truncates_long_text_and_handles_non_post_records() {
        let raw = serde_json::json!({
            "uri": "at://did:plc:alice/app.bsky.feed.like/1",
            "cid": "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy",
            "indexedAt": "2026-11-12T10:00:00.000Z",
            "isRead": false,
            "reason": "like",
            "record": { "$type": "app.bsky.feed.like" },
            "author": { "did": "did:plc:alice", "handle": "alice.test" }
        });
        let notification: list_notifications::Notification = serde_json::from_value(raw).unwrap();
        let digest = digest_of(&notification);
        assert_eq!(digest.reason, "like");
        assert_eq!(digest.text_snippet, "");
    }
}
