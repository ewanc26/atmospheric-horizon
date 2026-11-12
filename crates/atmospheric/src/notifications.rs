//! Notification polling with a persisted seen-cursor.

use serde::{Deserialize, Serialize};

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

/// Poll `app.bsky.notification.listNotifications` and return the new digests.
///
/// TODO(M2): wire to the agent; cursor state persists under the mod's storage
/// folder so restarts do not re-show old notifications.
pub async fn poll(
    _agent: &crate::session::Agent,
    _cursor: &mut SeenCursor,
) -> crate::error::Result<Vec<NotificationDigest>> {
    todo!("notification polling lands in M4")
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
}
