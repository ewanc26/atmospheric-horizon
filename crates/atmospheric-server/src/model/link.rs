use serde::{Deserialize, Serialize};

/// A player-to-ATProto link as stored and returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Canonical lowercase hyphenated Minecraft UUID.
    pub player_uuid: String,
    /// Last known Minecraft player name.
    pub player_name: String,
    /// The player's ATProto DID.
    pub did: String,
    /// The player's handle when known.
    pub handle: Option<String>,
    /// Unix seconds when the link was first created.
    pub linked_at: u64,
}

/// Body for an operator-created or operator-replaced link.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    pub player_name: String,
    pub did: String,
    #[serde(default)]
    pub handle: Option<String>,
}

/// Authenticated link listing.
#[derive(Debug, Serialize)]
pub struct LinksResponse {
    pub links: Vec<Link>,
    pub total: usize,
}
