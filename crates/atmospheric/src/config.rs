//! Mod configuration, stored under the mod's own settings folder.

use serde::{Deserialize, Serialize};

/// Where the mod may talk to. Nothing else: no telemetry, no other endpoints.
///
/// Credentials are app passwords, never account passwords. The config file
/// lives in the mod's own storage folder (see the Horizon mod storage API) —
/// not in a world save, and not next to one.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pds_url: "https://bsky.social".into(),
            identifier: String::new(),
            app_password: String::new(),
            poll_interval_secs: 60,
            post_death_messages: false,
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
        };
        let raw = config.to_json().unwrap();
        let back = Config::from_json(&raw).unwrap();
        assert_eq!(config.pds_url, back.pds_url);
        assert_eq!(config.poll_interval_secs, back.poll_interval_secs);
        assert!(back.post_death_messages);
    }

    #[test]
    fn empty_config_is_not_usable() {
        assert!(!Config::default().is_usable());
    }
}
