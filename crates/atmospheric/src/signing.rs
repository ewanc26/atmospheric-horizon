//! Ed25519 endorsement keys for `at.atpcraft.player.snapshot` records,
//! matching atpcraft's scheme (see its `auth/ServerSigningKey`):
//!
//! - the key id is the first 16 base64url (no padding) chars of SHA-256 over
//!   the X.509/SPKI encoding of the public key, and doubles as the record key;
//! - the endorsement signature is base64 (standard, no padding) in the
//!   record's `sig.$bytes` field.

use crate::error::{AtmosphericError, Result};
use base64::{Engine as _, engine::general_purpose};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};

pub use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The fixed SPKI prefix for an Ed25519 public key (RFC 8410):
/// `SEQUENCE { SEQUENCE { OID 1.3.101.112 }, BIT STRING }` header, followed
/// by the 32-byte key.
const SPKI_ED25519_PREFIX: &[u8] = &[
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The endorsement algorithm label atpcraft writes into records.
pub const ALG_ED25519: &str = "ed25519";

/// A persisted endorsement signing key: the 32-byte seed plus its key id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningKeyFile {
    /// Base64 (standard) of the 32-byte Ed25519 seed.
    pub seed_b64: String,
    /// The key id: first 16 base64url chars of SHA-256 over the SPKI encoding.
    pub key_id: String,
}

/// The X.509/SPKI encoding of an Ed25519 public key (RFC 8410 header + the
/// 32-byte key).
fn spki_bytes(verifying_key: &VerifyingKey) -> Vec<u8> {
    let mut spki = Vec::with_capacity(SPKI_ED25519_PREFIX.len() + 32);
    spki.extend_from_slice(SPKI_ED25519_PREFIX);
    spki.extend_from_slice(&verifying_key.to_bytes());
    spki
}

/// The key id of a verifying key: first 16 base64url (no padding) chars of
/// SHA-256 over its X.509/SPKI encoding.
pub fn key_id_of(verifying_key: &VerifyingKey) -> String {
    let digest = Sha256::digest(spki_bytes(verifying_key));
    general_purpose::URL_SAFE_NO_PAD.encode(digest)[..16].to_string()
}

/// The public key as base64 (standard, padded) of its X.509/SPKI encoding —
/// the `publicKey` field of `at.atpcraft.server.profile` records (atpcraft's
/// `Base64.getEncoder().encodeToString(key.publicKey().encoded)`).
pub fn public_key_spki_b64(verifying_key: &VerifyingKey) -> String {
    general_purpose::STANDARD.encode(spki_bytes(verifying_key))
}

/// Generate a fresh signing key (random seed).
pub fn generate() -> SigningKey {
    SigningKey::generate(&mut rand_core::OsRng)
}

/// The key file for a signing key.
pub fn key_file_of(key: &SigningKey) -> SigningKeyFile {
    SigningKeyFile {
        seed_b64: general_purpose::STANDARD.encode(key.to_bytes()),
        key_id: key_id_of(&key.verifying_key()),
    }
}

/// Rebuild a signing key from its persisted file.
pub fn from_key_file(file: &SigningKeyFile) -> Result<SigningKey> {
    let seed: [u8; 32] = general_purpose::STANDARD
        .decode(&file.seed_b64)
        .map_err(|e| AtmosphericError::Message(format!("bad signing key file: {e}")))?
        .try_into()
        .map_err(|_| AtmosphericError::Message("bad signing key file: seed length".into()))?;
    let key = SigningKey::from_bytes(&seed);
    if key_id_of(&key.verifying_key()) != file.key_id {
        return Err(AtmosphericError::Message(
            "signing key file does not match its key id".into(),
        ));
    }
    Ok(key)
}

/// Sign a payload; returns base64 (standard, no padding) as the record wants.
pub fn sign_b64(key: &SigningKey, payload: &[u8]) -> String {
    general_purpose::STANDARD_NO_PAD.encode(key.sign(payload).to_bytes())
}

/// Verify a base64 (standard, no padding) signature over a payload.
pub fn verify_b64(verifying_key: &VerifyingKey, payload: &[u8], sig_b64: &str) -> Result<()> {
    let sig_bytes = general_purpose::STANDARD_NO_PAD
        .decode(sig_b64)
        .map_err(|e| AtmosphericError::Message(format!("bad signature encoding: {e}")))?;
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|e| AtmosphericError::Message(format!("bad signature: {e}")))?;
    verifying_key
        .verify(payload, &signature)
        .map_err(|_| AtmosphericError::Message("signature does not verify".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_id_matches_rfc8032_vector() {
        // RFC 8032 Ed25519 test vector 1: this seed has this public key.
        let seed: [u8; 32] =
            hex_decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let key = SigningKey::from_bytes(&seed);
        // Cross-checked against openssl for this seed.
        let public: [u8; 32] =
            hex_decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        assert_eq!(key.verifying_key().to_bytes(), public);
        // The key id is 16 base64url chars derived from the SPKI encoding.
        let key_id = key_id_of(&key.verifying_key());
        assert_eq!(key_id.len(), 16);
        assert!(
            key_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        // Deterministic for the same key.
        assert_eq!(
            key_id,
            key_id_of(&SigningKey::from_bytes(&seed).verifying_key())
        );
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let key = generate();
        let payload = b"canonical payload bytes";
        let sig_b64 = sign_b64(&key, payload);
        assert_eq!(
            general_purpose::STANDARD_NO_PAD
                .decode(&sig_b64)
                .unwrap()
                .len(),
            64
        );
        verify_b64(&key.verifying_key(), payload, &sig_b64).unwrap();
        // Tampering fails.
        assert!(verify_b64(&key.verifying_key(), b"other", &sig_b64).is_err());
    }

    #[test]
    fn key_file_round_trips_and_detects_tampering() {
        let key = generate();
        let file = key_file_of(&key);
        let raw = serde_json::to_string(&file).unwrap();
        let back = from_key_file(&serde_json::from_str::<SigningKeyFile>(&raw).unwrap()).unwrap();
        assert_eq!(back.verifying_key(), key.verifying_key());

        let mut tampered = file.clone();
        tampered.key_id = "0000000000000000".into();
        assert!(from_key_file(&tampered).is_err());
    }

    fn hex_decode(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            out[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap();
        }
        out
    }
}
