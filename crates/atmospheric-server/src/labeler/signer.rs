//! K-256 (secp256k1) label keys and signatures.
//!
//! A label signature is a 64-byte compact ECDSA signature (`r || s`, low-S)
//! over the SHA-256 digest of the canonical CBOR form of the unsigned label.
//! The public key is published in the labeler's DID document as a `did:key`
//! multibase value with the secp256k1 multicodec prefix.

use super::{cbor, label::Label};
use ::k256::ecdsa::{
    Signature, SigningKey,
    signature::hazmat::{PrehashSigner, PrehashVerifier},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The secp256k1 multicodec prefix used by `did:key`.
const SECP256K1_MULTICODEC: [u8; 2] = [0xe7, 0x01];

/// A label signing key.
#[derive(Debug, Error)]
pub enum LabelError {
    #[error("the label signing key is invalid")]
    InvalidKey,
}

/// A K-256 key that signs and verifies `com.atproto.label.defs#label` values.
#[derive(Clone)]
pub struct LabelSigner {
    key: SigningKey,
}

impl LabelSigner {
    /// Create a key from a 32-byte big-endian secret scalar.
    pub fn from_secret(secret: [u8; 32]) -> Result<Self, LabelError> {
        SigningKey::from_bytes((&secret).into())
            .map(|key| Self { key })
            .map_err(|_| LabelError::InvalidKey)
    }

    /// Generate a fresh key from the operating system RNG.
    pub fn generate() -> Self {
        let mut rng = rand_core::OsRng;
        Self {
            key: SigningKey::random(&mut rng),
        }
    }

    /// The secret scalar, for owner-only key-file persistence.
    pub fn secret_bytes(&self) -> [u8; 32] {
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&self.key.to_bytes());
        secret
    }

    /// The compressed public key bytes (33 bytes).
    pub fn public_key_bytes(&self) -> [u8; 33] {
        self.key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .expect("a compressed secp256k1 key is 33 bytes")
    }

    /// The compressed public key as lowercase hex (33 bytes).
    pub fn public_key_hex(&self) -> String {
        let point = self.key.verifying_key().to_encoded_point(true);
        let mut out = String::with_capacity(66);
        for byte in point.as_bytes() {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// The public key as a multibase `did:key` (`did:key:zQ3sh…`).
    pub fn did_key(&self) -> String {
        let point = self.key.verifying_key().to_encoded_point(true);
        let mut bytes = Vec::with_capacity(35);
        bytes.extend_from_slice(&SECP256K1_MULTICODEC);
        bytes.extend_from_slice(point.as_bytes());
        format!("did:key:z{}", bs58::encode(bytes).into_string())
    }

    /// Sign a label, returning its 64-byte compact low-S signature.
    pub fn sign(&self, label: &Label) -> [u8; 64] {
        let digest = Sha256::digest(cbor::encode_label(label));
        let signature: Signature = self
            .key
            .sign_prehash(&digest)
            .expect("a 32-byte digest is always signable");
        let signature = signature.normalize_s().unwrap_or(signature);
        signature.to_bytes().into()
    }

    /// Verify a label's 64-byte signature with this key.
    pub fn verify(&self, label: &Label, signature: &[u8; 64]) -> bool {
        let Ok(signature) = Signature::from_slice(signature) else {
            return false;
        };
        let digest = Sha256::digest(cbor::encode_label(label));
        self.key
            .verifying_key()
            .verify_prehash(&digest, &signature)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::k256::ecdsa::Signature;

    fn key_one() -> LabelSigner {
        let mut secret = [0u8; 32];
        secret[31] = 1;
        LabelSigner::from_secret(secret).unwrap()
    }

    fn sample() -> Label {
        Label {
            ver: 1,
            src: "did:plc:labeler".into(),
            uri: "did:plc:alice".into(),
            cid: None,
            val: "online".into(),
            neg: false,
            cts: "2026-11-14T00:00:00.000Z".into(),
            exp: None,
        }
    }

    #[test]
    fn key_one_matches_the_secp256k1_generator_point() {
        let signer = key_one();
        assert_eq!(
            signer.public_key_hex(),
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );
        let did_key = signer.did_key();
        assert!(did_key.starts_with("did:key:zQ3sh"), "{did_key}");
        let encoded = bs58::decode(did_key.trim_start_matches("did:key:z"))
            .into_vec()
            .unwrap();
        assert_eq!(&encoded[..2], &SECP256K1_MULTICODEC);
        assert_eq!(encoded[2..].len(), 33);
    }

    #[test]
    fn the_secret_round_trips_and_is_validated() {
        let signer = key_one();
        assert_eq!(signer.secret_bytes()[31], 1);
        assert!(LabelSigner::from_secret([0u8; 32]).is_err());
    }

    #[test]
    fn signatures_are_deterministic_compact_and_low_s() {
        let signer = key_one();
        let label = sample();
        let first = signer.sign(&label);
        let second = signer.sign(&label);
        assert_eq!(first, second);
        let signature = Signature::from_slice(&first).unwrap();
        assert!(
            signature.normalize_s().is_none(),
            "signing must produce the low-S form"
        );
        assert!(signer.verify(&label, &first));
    }

    #[test]
    fn verification_rejects_tampering_and_other_keys() {
        let signer = key_one();
        let label = sample();
        let signature = signer.sign(&label);

        let mut changed = label.clone();
        changed.val = "admin".into();
        assert!(!signer.verify(&changed, &signature));
        assert!(!signer.verify(&label, &[0u8; 64]));

        let other = LabelSigner::generate();
        assert!(!other.verify(&label, &signature));
    }
}
