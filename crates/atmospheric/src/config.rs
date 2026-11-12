//! Mod configuration, stored under the mod's own settings folder.

use serde::{Deserialize, Serialize};

/// Where the mod may talk to. Nothing else: no telemetry, no other endpoints.
///
/// Credentials are app passwords, never account passwords. The config file
/// lives in the mod's own storage folder (see the Horizon mod storage API) —
/// not in a world save, and not next to one.
/// The default `chat_server_id` written into `at.atpcraft.player.chat`
/// records: this client mod has no server identity of its own.
fn default_chat_server_id() -> String {
    "horizon-client".into()
}

// `#[serde(default)]` keeps settings files written by older versions (which
// lack the newer fields) loadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// PDS URL, e.g. `https://bsky.social` or a self-hosted PDS.
    pub pds_url: String,
    /// Handle, email, or DID used as the login identifier.
    pub identifier: String,
    /// A Bluesky app password.
    pub app_password: String,
    /// Seconds between notification polls.
    pub poll_interval_secs: u64,
    /// Post the death message automatically when the player dies.
    pub post_death_messages: bool,
    /// Publish every chat message as an `at.atpcraft.player.chat` record.
    /// Off by default: chat is public once written to the repo.
    pub post_chat_messages: bool,
    /// The `server` field written into chat records (atpcraft port).
    pub chat_server_id: String,
    /// Persist the session's refresh token so restarts resume without the
    /// app password (`session.json` in the mod's storage folder).
    pub persist_session: bool,
    /// The OAuth `client_id` (M8). Empty means the default loopback form
    /// (`http://localhost?client_id=…`); set it to a hosted client-metadata
    /// URL (see `oauth-client-metadata.json` in the repo root) for
    /// authorization servers that do not implement the loopback exception.
    pub oauth_client_id: String,
    /// Path of the player's skin PNG used by `/atmospheric skin` when the
    /// command gets no explicit path (M9). Empty means "not configured".
    pub skin_path: String,
    /// Optional path of the player's cape PNG (M9).
    pub cape_path: String,
    /// The skin model written into `at.atpcraft.player.skin` records (M9):
    /// "default" or "slim".
    pub skin_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pds_url: "https://bsky.social".into(),
            identifier: String::new(),
            app_password: String::new(),
            poll_interval_secs: 60,
            post_death_messages: false,
            post_chat_messages: false,
            chat_server_id: default_chat_server_id(),
            persist_session: true,
            oauth_client_id: String::new(),
            skin_path: String::new(),
            cape_path: String::new(),
            skin_model: "default".into(),
        }
    }
}

impl Config {
    /// Serialise for the settings file.
    pub fn to_json(&self) -> crate::error::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Deserialise from the settings file.
    pub fn from_json(raw: &str) -> crate::error::Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }

    /// Whether there is enough configured for a login attempt.
    pub fn is_usable(&self) -> bool {
        !self.identifier.is_empty() && !self.app_password.is_empty()
    }

    /// The OAuth client_id to use: the configured hosted metadata URL when
    /// set, otherwise the loopback default for the given scope.
    pub fn oauth_client_id(&self, scope: &str) -> String {
        if self.oauth_client_id.is_empty() {
            crate::oauth::client_id(scope)
        } else {
            self.oauth_client_id.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_through_json() {
        let config = Config {
            pds_url: "https://pds.example".into(),
            identifier: "ewan.example".into(),
            app_password: "abcd-1234-efgh-5678".into(),
            poll_interval_secs: 30,
            post_death_messages: true,
            post_chat_messages: true,
            chat_server_id: "my-server".into(),
            persist_session: true,
            oauth_client_id: String::new(),
            skin_path: String::new(),
            cape_path: String::new(),
            skin_model: "default".into(),
        };
        let raw = config.to_json().unwrap();
        let back = Config::from_json(&raw).unwrap();
        assert_eq!(config.pds_url, back.pds_url);
        assert_eq!(config.poll_interval_secs, back.poll_interval_secs);
        assert!(back.post_death_messages);
        assert!(back.post_chat_messages);
        assert_eq!(back.chat_server_id, "my-server");
        assert!(back.persist_session);
    }

    #[test]
    fn config_from_an_older_settings_file_gets_the_new_defaults() {
        // A settings file written before the atpcraft-port fields existed.
        let raw = r#"{
            "pds_url": "https://pds.example",
            "identifier": "ewan.example",
            "app_password": "abcd-1234-efgh-5678",
            "poll_interval_secs": 30,
            "post_death_messages": false
        }"#;
        let config = Config::from_json(raw).unwrap();
        assert!(
            !config.post_chat_messages,
            "chat publishing defaults to off"
        );
        assert_eq!(config.chat_server_id, "horizon-client");
    }

    #[test]
    fn empty_config_is_not_usable() {
        assert!(!Config::default().is_usable());
    }
}
