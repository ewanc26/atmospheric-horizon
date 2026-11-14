use super::encoding::{b64url, random_token};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use sha2::{Digest, Sha256};

fn htu(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// An ES256 key used for DPoP proofs. It lives only for one browser login.
#[derive(Clone)]
pub struct DpopKey {
    secret: p256::SecretKey,
}

impl DpopKey {
    pub fn generate() -> Self {
        Self {
            secret: p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng),
        }
    }

    fn signing_key(&self) -> SigningKey {
        SigningKey::from(&self.secret)
    }

    fn verifying_key(&self) -> VerifyingKey {
        *self.signing_key().verifying_key()
    }

    fn public_jwk(&self) -> serde_json::Value {
        let point = self.verifying_key().to_encoded_point(false);
        let bytes = point.as_bytes();
        serde_json::json!({
            "crv": "P-256",
            "kty": "EC",
            "x": b64url(&bytes[1..33]),
            "y": b64url(&bytes[33..65]),
        })
    }

    pub fn thumbprint(&self) -> String {
        let jwk = self.public_jwk();
        let canonical = format!(
            "{{\"crv\":\"{}\",\"kty\":\"{}\",\"x\":\"{}\",\"y\":\"{}\"}}",
            jwk["crv"].as_str().unwrap_or_default(),
            jwk["kty"].as_str().unwrap_or_default(),
            jwk["x"].as_str().unwrap_or_default(),
            jwk["y"].as_str().unwrap_or_default(),
        );
        b64url(&Sha256::digest(canonical.as_bytes()))
    }

    pub fn proof(
        &self,
        htm: &str,
        htu_url: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<String, String> {
        let header = serde_json::json!({
            "typ": "dpop+jwt",
            "alg": "ES256",
            "jwk": self.public_jwk(),
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut claims = serde_json::json!({
            "jti": random_token(16),
            "htm": htm,
            "htu": htu(htu_url),
            "iat": now,
        });
        if let Some(nonce) = nonce {
            claims["nonce"] = serde_json::Value::String(nonce.into());
        }
        if let Some(access_token) = access_token {
            claims["ath"] =
                serde_json::Value::String(b64url(&Sha256::digest(access_token.as_bytes())));
        }
        let head = b64url(&serde_json::to_vec(&header).map_err(|e| e.to_string())?);
        let payload = b64url(&serde_json::to_vec(&claims).map_err(|e| e.to_string())?);
        let signing_input = format!("{head}.{payload}");
        let signature: Signature = self.signing_key().sign(signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", b64url(&signature.to_bytes())))
    }
}

/// A decoded JWS header/claims pair, for tests and diagnostics.
#[cfg(test)]
fn decode_jws(token: &str) -> (serde_json::Value, serde_json::Value) {
    let mut parts = token.split('.');
    let header =
        serde_json::from_slice(&super::encoding::b64url_decode(parts.next().unwrap()).unwrap())
            .unwrap();
    let claims =
        serde_json::from_slice(&super::encoding::b64url_decode(parts.next().unwrap()).unwrap())
            .unwrap();
    (header, claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpop_proof_is_an_es256_jwt_with_the_request_binding() {
        let key = DpopKey::generate();
        let proof = key
            .proof(
                "POST",
                "https://auth.example/oauth/par?ignored=1",
                Some("nonce-1"),
                None,
            )
            .unwrap();
        let (header, claims) = decode_jws(&proof);
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(claims["htm"], "POST");
        assert_eq!(claims["htu"], "https://auth.example/oauth/par");
        assert_eq!(claims["nonce"], "nonce-1");
        assert!(header["jwk"]["x"].is_string());
        assert_eq!(header["jwk"]["crv"], "P-256");
        assert_eq!(key.thumbprint().len(), 43);
    }
}
