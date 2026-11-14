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

/// One trusted atpcraft server endorsement key (M13 presence reads): the
/// base64 (standard) X.509/SPKI public key, as published in the server's
/// `at.atpcraft.server.profile` record, plus the key id — derived from the
/// public key with the same algorithm as the mod's own key when blank
/// (matching atpcraft's `TrustedServerKeys` entries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TrustedServerKey {
    #[serde(default)]
    pub key_id: String,
    pub public_key: String,
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
    /// Server-profile identity fields (M10); blank means "omit from the
    /// record", except `server_name`, which falls back to the login handle.
    pub server_name: String,
    pub server_description: String,
    pub server_address: String,
    pub server_website: String,
    pub server_contact: String,
    /// The `capabilities` list of the server profile (M10); sorted and
    /// deduped on write.
    pub server_capabilities: Vec<String>,
    /// The `minecraftVersion` written into the server profile (M10).
    pub game_version: String,
    /// The AppView used for public list reads (M11): `app.bsky.graph.getList`
    /// against this base, unauthenticated. atpcraft uses the public AppView.
    pub appview_url: String,
    /// Base URL of the optional M35 identity service (M36). Empty disables
    /// `/atmospheric link`; no request is made unless that command is used.
    pub server_api_url: String,
    /// Shared secret the game server sends to `POST /v1/browser/pairings` when
    /// a player starts a browser link (M39). Empty disables the browser flow;
    /// it is a server-only secret and should never be published.
    pub server_link_token: String,
    /// Enforce the join gate at the server login phase (M38). A joining
    /// player's UUID must resolve through `server_api_url` to a linked DID on
    /// `join_gate_list`. Off by default. When on, an empty list or an
    /// unavailable identity/AppView read refuses the join (fail-closed).
    pub join_gate_enabled: bool,
    /// The public Bluesky list used by the join gate (M38): an
    /// `at://…/app.bsky.graph.list/<rkey>` URI or a bsky.app list URL.
    pub join_gate_list: String,
    /// How often the online whitelist sweep re-reads `join_gate_list` and
    /// disconnects a linked player whose DID has left it (M42). Seconds; `0`
    /// disables the sweep. The worker clamps it to at least 60 seconds when it
    /// is on, matching atpcraft's list-cache floor.
    pub join_gate_sweep_seconds: u64,
    /// Trusted atpcraft server endorsement keys (M13 presence reads): when
    /// reading a player's `at.atpcraft.player.snapshot` record, endorsements
    /// from these key ids are accepted alongside the mod's own signing key.
    pub trusted_server_keys: Vec<TrustedServerKey>,
    /// Arm the Colibri chat bridge automatically once the server account is
    /// available (M26). The command can start it explicitly too.
    pub colibri_bridge_enabled: bool,
    /// The channel the bridge joins: a Colibri channel AT-URI or app link.
    pub colibri_channel_uri: String,
    /// Base URL of the Colibri AppView (M26).
    pub colibri_appview_url: String,
    /// Service DID of the AppView; `#colibri_appview` is appended for the
    /// service-auth audience unless the value already carries a fragment.
    pub colibri_appview_did: String,
    /// Relay server join/leave/death/advancement broadcasts into the bridged
    /// channel as the server account (M28). Default on, matching atpcraft.
    pub colibri_server_events_enabled: bool,
    /// When a chat mod rewrites or hides the vanilla join/leave line, use the
    /// connection event as a fallback after a short wait (M29). Default on.
    pub colibri_server_events_fallback_enabled: bool,
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
            server_name: String::new(),
            server_description: String::new(),
            server_address: String::new(),
            server_website: String::new(),
            server_contact: String::new(),
            server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
            game_version: "26.3".into(),
            appview_url: crate::lists::DEFAULT_APPVIEW.into(),
            server_api_url: String::new(),
            server_link_token: String::new(),
            join_gate_enabled: false,
            join_gate_list: String::new(),
            join_gate_sweep_seconds: 60,
            trusted_server_keys: Vec::new(),
            colibri_bridge_enabled: false,
            colibri_channel_uri: String::new(),
            colibri_appview_url: "https://api.colibri.social".into(),
            colibri_appview_did: "did:web:api.colibri.social".into(),
            colibri_server_events_enabled: true,
            colibri_server_events_fallback_enabled: true,
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
            server_name: String::new(),
            server_description: String::new(),
            server_address: String::new(),
            server_website: String::new(),
            server_contact: String::new(),
            server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
            game_version: "26.3".into(),
            appview_url: crate::lists::DEFAULT_APPVIEW.into(),
            server_api_url: "https://identity.example".into(),
            server_link_token: "server-link-secret".into(),
            join_gate_enabled: true,
            join_gate_list: "at://did:plc:owner/app.bsky.graph.list/3gate".into(),
            join_gate_sweep_seconds: 120,
            trusted_server_keys: vec![TrustedServerKey {
                key_id: String::new(),
                public_key: "MCowBQYDK2VwAyEA...".into(),
            }],
            colibri_bridge_enabled: true,
            colibri_channel_uri: "https://colibri.social/app/c/did:plc:abc/text/3jtest".into(),
            colibri_appview_url: "https://appview.example".into(),
            colibri_appview_did: "did:web:appview.example".into(),
            colibri_server_events_enabled: false,
            colibri_server_events_fallback_enabled: false,
        };
        let raw = config.to_json().unwrap();
        let back = Config::from_json(&raw).unwrap();
        assert_eq!(config.pds_url, back.pds_url);
        assert_eq!(config.poll_interval_secs, back.poll_interval_secs);
        assert!(back.post_death_messages);
        assert!(back.post_chat_messages);
        assert_eq!(back.chat_server_id, "my-server");
        assert!(back.persist_session);
        assert_eq!(back.trusted_server_keys, config.trusted_server_keys);
        assert!(back.colibri_bridge_enabled);
        assert_eq!(back.colibri_channel_uri, config.colibri_channel_uri);
        assert_eq!(back.colibri_appview_url, "https://appview.example");
        assert_eq!(back.server_api_url, "https://identity.example");
        assert_eq!(back.server_link_token, "server-link-secret");
        assert!(back.join_gate_enabled);
        assert_eq!(
            back.join_gate_list,
            "at://did:plc:owner/app.bsky.graph.list/3gate"
        );
        assert_eq!(back.join_gate_sweep_seconds, 120);
        assert!(!back.colibri_server_events_enabled);
        assert!(!back.colibri_server_events_fallback_enabled);
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
        assert!(config.trusted_server_keys.is_empty());
        assert!(!config.colibri_bridge_enabled);
        assert!(config.colibri_channel_uri.is_empty());
        assert!(!config.join_gate_enabled);
        assert!(config.join_gate_list.is_empty());
        assert_eq!(config.join_gate_sweep_seconds, 60);
        assert_eq!(config.colibri_appview_url, "https://api.colibri.social");
        assert!(config.server_api_url.is_empty());
        assert!(config.colibri_server_events_fallback_enabled);
    }

    #[test]
    fn empty_config_is_not_usable() {
        assert!(!Config::default().is_usable());
    }
}
