use crate::error::ApiError;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

/// Default lifetime of a pairing code: ten minutes.
pub const DEFAULT_PAIRING_TTL_SECS: u64 = 600;
/// The longest accepted pairing-code lifetime: one hour.
pub const MAX_PAIRING_TTL_SECS: u64 = 3_600;
/// The shortest accepted pairing-code lifetime.
pub const MIN_PAIRING_TTL_SECS: u64 = 60;

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
    matches!(byte, b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'2'..=b'9')
}

#[cfg(test)]
mod tests {
    use super::*;

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
