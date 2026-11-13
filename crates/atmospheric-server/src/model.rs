//! Wire types and the validation/normalisation rules shared by the API and
//! the store. Everything here is pure: no I/O, no HTTP.

use crate::error::ApiError;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

/// Default lifetime of a pairing code: ten minutes.
pub const DEFAULT_PAIRING_TTL_SECS: u64 = 600;
/// The longest accepted pairing-code lifetime: one hour.
pub const MAX_PAIRING_TTL_SECS: u64 = 3_600;
/// The shortest accepted pairing-code lifetime.
pub const MIN_PAIRING_TTL_SECS: u64 = 60;

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

/// Body for creating a one-time pairing code.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingInput {
    pub player_uuid: String,
    pub player_name: String,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

/// The code returned to an operator. It is shown once and stored only as a
/// pending claim until it is used or expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingResponse {
    pub code: String,
    pub expires_at: u64,
}

/// Body a player submits to consume a pairing code.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimInput {
    pub code: String,
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

/// Unauthenticated service health and collection counts.
#[derive(Debug, Serialize)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
    pub links: usize,
    pub pending_pairings: usize,
}

/// Canonicalise a Minecraft UUID. Accepts the hyphenated or compact form,
/// upper or lower case, and always returns lowercase 8-4-4-4-12.
pub fn normalize_uuid(value: &str) -> Result<String, ApiError> {
    let compact: String = value.chars().filter(|c| *c != '-').collect();
    if compact.len() != 32 || !compact.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::Invalid(
            "player_uuid must be a 32-hex-digit Minecraft UUID".into(),
        ));
    }
    let compact = compact.to_ascii_lowercase();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &compact[0..8],
        &compact[8..12],
        &compact[12..16],
        &compact[16..20],
        &compact[20..32]
    ))
}

/// Validate a Minecraft username.
pub fn normalize_player_name(value: &str) -> Result<String, ApiError> {
    let name = value.trim();
    if name.is_empty()
        || name.chars().count() > 16
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(ApiError::Invalid(
            "player_name must be 1-16 ASCII letters, digits or underscores".into(),
        ));
    }
    Ok(name.to_owned())
}

/// Validate an ATProto DID. The server only needs the shape; resolution is the
/// client's job, so this deliberately does not make network requests.
pub fn normalize_did(value: &str) -> Result<String, ApiError> {
    let did = value.trim();
    let supported = did.starts_with("did:plc:") || did.starts_with("did:web:");
    let suffix = did
        .strip_prefix("did:plc:")
        .or_else(|| did.strip_prefix("did:web:"))
        .unwrap_or("");
    if !supported
        || suffix.is_empty()
        || did.len() > 2_048
        || did.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ApiError::Invalid(
            "did must be a non-empty did:plc or did:web identifier".into(),
        ));
    }
    Ok(did.to_owned())
}

/// Normalise an optional handle. A blank handle means "unknown".
pub fn normalize_handle(value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let handle = value.trim().to_ascii_lowercase();
    if handle.is_empty() {
        return Ok(None);
    }
    let valid = handle.len() <= 253
        && handle.contains('.')
        && handle.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if !valid {
        return Err(ApiError::Invalid(
            "handle must be a domain-style ATProto handle".into(),
        ));
    }
    Ok(Some(handle))
}

/// Resolve the requested pairing lifetime, clamped to the accepted range.
pub fn pairing_ttl(value: Option<u64>) -> Result<u64, ApiError> {
    let ttl = value.unwrap_or(DEFAULT_PAIRING_TTL_SECS);
    if !(MIN_PAIRING_TTL_SECS..=MAX_PAIRING_TTL_SECS).contains(&ttl) {
        return Err(ApiError::Invalid(format!(
            "ttl_seconds must be between {MIN_PAIRING_TTL_SECS} and {MAX_PAIRING_TTL_SECS}"
        )));
    }
    Ok(ttl)
}

/// Canonicalise a user-supplied pairing code: case-insensitive, with hyphens
/// and surrounding whitespace ignored.
pub fn normalize_code(value: &str) -> Result<String, ApiError> {
    let code: String = value
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if code.len() != 8 || !code.bytes().all(is_pairing_byte) {
        return Err(ApiError::Invalid(
            "code must be an eight-character pairing code".into(),
        ));
    }
    Ok(code)
}

/// Generate a new code in `XXXX-XXXX` form. The alphabet omits the easily
/// confused I, O, 0 and 1.
pub(crate) fn new_pairing_code() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    let mut code = String::with_capacity(9);
    for (index, byte) in bytes.iter().enumerate() {
        if index == 4 {
            code.push('-');
        }
        code.push(ALPHABET[(byte & 31) as usize] as char);
    }
    code
}

fn is_pairing_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'2'..=b'9'
    )
}

/// Current Unix time in seconds. A clock before the epoch is treated as zero;
/// the server does not need to fail over a broken clock.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_is_canonicalised() {
        assert_eq!(
            normalize_uuid("00000000000040008000000000000001").unwrap(),
            "00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(
            normalize_uuid("00000000-0000-4000-8000-000000000001").unwrap(),
            "00000000-0000-4000-8000-000000000001"
        );
        assert!(normalize_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn names_dids_and_handles_are_validated() {
        assert_eq!(normalize_player_name(" Alex_1 ").unwrap(), "Alex_1");
        assert!(normalize_player_name("").is_err());
        assert!(normalize_player_name("Alex!").is_err());

        assert_eq!(normalize_did(" did:plc:abc123 ").unwrap(), "did:plc:abc123");
        assert!(normalize_did("did:key:abc").is_err());
        assert!(normalize_did("did:plc:").is_err());

        assert_eq!(
            normalize_handle(Some(" Alice.Test ")).unwrap(),
            Some("alice.test".into())
        );
        assert_eq!(normalize_handle(Some("  ")).unwrap(), None);
        assert!(normalize_handle(Some("alice")).is_err());
        assert!(normalize_handle(Some("alice..test")).is_err());
    }

    #[test]
    fn pairing_codes_are_normalised_and_formatted() {
        let code = new_pairing_code();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        let normalised = normalize_code(&code).unwrap();
        assert_eq!(normalised.len(), 8);
        assert_eq!(normalize_code(&code.to_lowercase()).unwrap(), normalised);
        assert!(normalize_code("short").is_err());
        assert!(normalize_code("AAAA-AAA0").is_err());
    }

    #[test]
    fn pairing_ttl_is_bounded() {
        assert_eq!(pairing_ttl(None).unwrap(), DEFAULT_PAIRING_TTL_SECS);
        assert_eq!(pairing_ttl(Some(60)).unwrap(), 60);
        assert!(pairing_ttl(Some(59)).is_err());
        assert!(pairing_ttl(Some(MAX_PAIRING_TTL_SECS + 1)).is_err());
    }
}
