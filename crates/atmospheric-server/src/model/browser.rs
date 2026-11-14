use serde::{Deserialize, Serialize};

/// Body the game server sends to start a browser link for one online player.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPairingInput {
    pub player_uuid: String,
    pub player_name: String,
    /// Requested link lifetime in seconds. Omitted uses the service default;
    /// the value is bounded by the same range as operator pairing codes.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

/// The one-time browser URL handed back to the game server. The URL's token
/// is the bearer credential shown only to the player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserPairingResponse {
    pub url: String,
    pub expires_at: u64,
}
