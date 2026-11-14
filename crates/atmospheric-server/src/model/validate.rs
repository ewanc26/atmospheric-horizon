use crate::error::ApiError;

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
}
