use base64::Engine as _;
use sha2::{Digest, Sha256};

pub(super) fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
pub(super) fn b64url_decode(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|error| format!("bad base64url: {error}"))
}

pub(super) fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub(crate) fn random_token(bytes: usize) -> String {
    use p256::elliptic_curve::rand_core::{OsRng, RngCore};
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    b64url(&buf)
}

pub(super) fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

pub(super) fn normalize_base(base: &str) -> String {
    base.trim().trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_deterministic_base64url() {
        let challenge = pkce_challenge("verifier");
        assert_eq!(challenge, pkce_challenge("verifier"));
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
        assert_ne!(challenge, pkce_challenge("other"));
    }
}
