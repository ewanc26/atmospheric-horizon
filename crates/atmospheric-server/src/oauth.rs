//! The public-client AT Protocol OAuth flow used by the browser link page.
//!
//! This mirrors the spec (PAR, PKCE S256, DPoP-bound tokens). Unlike the mod's
//! loopback login, the redirect is a public HTTPS URL, so the service publishes
//! client metadata at `/oauth-client-metadata.json` and uses that URL as the
//! client id.

use base64::Engine as _;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use sha2::{Digest, Sha256};

pub const SCOPE: &str = "atproto transition:generic";

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
fn b64url_decode(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|error| format!("bad base64url: {error}"))
}

fn url_encode(text: &str) -> String {
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

fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn htu(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

fn normalize_base(base: &str) -> String {
    base.trim().trim_end_matches('/').to_owned()
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

/// Where handles and DIDs resolve. The defaults are Bluesky's public services;
/// tests point them at a stub.
#[derive(Debug, Clone)]
pub struct Resolver {
    pub handle_service: String,
    pub plc_directory: String,
}

impl Default for Resolver {
    fn default() -> Self {
        Self {
            handle_service: "https://bsky.social".into(),
            plc_directory: "https://plc.directory".into(),
        }
    }
}

/// The one-time material kept between PAR and the callback.
#[derive(Clone)]
pub struct PendingAuthorization {
    pub state: String,
    pub verifier: String,
    pub key: DpopKey,
    pub token_endpoint: String,
    pub issuer: String,
    pub did: String,
    pub handle: String,
}

pub struct Started {
    pub authorize_url: String,
    pub pending: PendingAuthorization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub did: String,
    pub handle: String,
}

#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    resolver: Resolver,
    client_id: String,
    redirect_uri: String,
}

impl OAuthClient {
    pub fn new(public_url: &str, resolver: Resolver) -> Self {
        let base = normalize_base(public_url);
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            resolver,
            client_id: format!("{base}/oauth-client-metadata.json"),
            redirect_uri: format!("{base}/link/callback"),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Resolve a handle and push an authorization request. The returned URL is
    /// the browser's next stop; `pending` belongs in the one-time session map.
    pub async fn start(&self, handle: &str) -> Result<Started, String> {
        let handle = handle.trim().trim_start_matches('@').to_ascii_lowercase();
        if handle.is_empty() || !handle.contains('.') {
            return Err("enter a Bluesky handle such as alice.bsky.social".into());
        }
        let did = self.resolve_handle(&handle).await?;
        let pds = self.resolve_pds(&did).await?;
        let (issuer, authorize_endpoint, token_endpoint, par_endpoint) =
            self.authorization_server(&pds).await?;
        let state = random_token(24);
        let verifier = random_token(32);
        let key = DpopKey::generate();
        let form = vec![
            ("response_type".to_string(), "code".to_string()),
            ("client_id".to_string(), self.client_id.clone()),
            ("redirect_uri".to_string(), self.redirect_uri.clone()),
            ("scope".to_string(), SCOPE.to_string()),
            ("state".to_string(), state.clone()),
            ("code_challenge".to_string(), pkce_challenge(&verifier)),
            ("code_challenge_method".to_string(), "S256".to_string()),
            ("login_hint".to_string(), handle.clone()),
            ("dpop_jkt".to_string(), key.thumbprint()),
        ];
        let (response, _) = self
            .post_form(&par_endpoint, &form, &key, None)
            .await
            .map_err(|error| format!("authorization request failed: {error}"))?;
        let request_uri = response
            .get("request_uri")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "authorization server returned no request_uri".to_string())?;
        let authorize_url = format!(
            "{authorize_endpoint}?client_id={}&request_uri={}",
            url_encode(&self.client_id),
            url_encode(request_uri)
        );
        Ok(Started {
            authorize_url,
            pending: PendingAuthorization {
                state,
                verifier,
                key,
                token_endpoint,
                issuer,
                did,
                handle,
            },
        })
    }

    /// Exchange the callback code and validate the DPoP-bound token identity.
    pub async fn finish(
        &self,
        pending: &PendingAuthorization,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<Identity, String> {
        let issuer = issuer
            .map(str::trim)
            .filter(|issuer| !issuer.is_empty())
            .ok_or_else(|| "the authorization response has no issuer".to_string())?;
        if normalize_base(issuer) != normalize_base(&pending.issuer) {
            return Err(
                "the authorization server does not match the one that started this login".into(),
            );
        }
        let form = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code.to_string()),
            ("redirect_uri".to_string(), self.redirect_uri.clone()),
            ("client_id".to_string(), self.client_id.clone()),
            ("code_verifier".to_string(), pending.verifier.clone()),
        ];
        let (response, _) = self
            .post_form(&pending.token_endpoint, &form, &pending.key, None)
            .await
            .map_err(|error| format!("token exchange failed: {error}"))?;
        let token_type = response
            .get("token_type")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if token_type != "DPoP" {
            return Err("the authorization server did not return a DPoP token".into());
        }
        let did = response
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if did != pending.did {
            return Err("the signed-in account did not match the requested handle".into());
        }
        let scope = response
            .get("scope")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if !scope.split_whitespace().any(|grant| grant == "atproto") {
            return Err("the authorization server did not grant the atproto scope".into());
        }
        Ok(Identity {
            did: did.to_string(),
            handle: pending.handle.clone(),
        })
    }

    async fn resolve_handle(&self, handle: &str) -> Result<String, String> {
        let base = normalize_base(&self.resolver.handle_service);
        let url = format!(
            "{base}/xrpc/com.atproto.identity.resolveHandle?handle={}",
            url_encode(handle)
        );
        let response = get_json(&self.http, &url).await?;
        response
            .get("did")
            .and_then(|value| value.as_str())
            .filter(|did| did.starts_with("did:"))
            .map(str::to_owned)
            .ok_or_else(|| "the handle did not resolve to a DID".into())
    }

    async fn resolve_pds(&self, did: &str) -> Result<String, String> {
        let url = if let Some(rest) = did.strip_prefix("did:plc:") {
            format!("{}/{}", normalize_base(&self.resolver.plc_directory), rest)
        } else if let Some(domain) = did.strip_prefix("did:web:") {
            let mut parts = domain.split(':');
            let host = parts.next().unwrap_or_default();
            let path: Vec<&str> = parts.collect();
            if path.is_empty() {
                format!("https://{host}/.well-known/did.json")
            } else {
                format!("https://{host}/{}/did.json", path.join("/"))
            }
        } else {
            return Err("only did:plc and did:web accounts are supported".into());
        };
        let document = get_json(&self.http, &url).await?;
        let services = document
            .get("service")
            .and_then(|value| value.as_array())
            .ok_or_else(|| "the DID document has no services".to_string())?;
        for service in services {
            if service.get("id").and_then(|value| value.as_str()) == Some("#atproto_pds") {
                let endpoint = service
                    .get("serviceEndpoint")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if endpoint.starts_with("https://") || endpoint.starts_with("http://") {
                    return Ok(normalize_base(endpoint));
                }
            }
        }
        Err("the DID document has no PDS service".into())
    }

    async fn authorization_server(
        &self,
        pds: &str,
    ) -> Result<(String, String, String, String), String> {
        let protected = get_json(
            &self.http,
            &format!(
                "{}/.well-known/oauth-protected-resource",
                normalize_base(pds)
            ),
        )
        .await?;
        let issuer = protected
            .get("authorization_servers")
            .and_then(|value| value.as_array())
            .and_then(|servers| servers.first())
            .and_then(|value| value.as_str())
            .filter(|issuer| !issuer.is_empty())
            .ok_or_else(|| "the PDS advertised no authorization server".to_string())?;
        let metadata = get_json(
            &self.http,
            &format!(
                "{}/.well-known/oauth-authorization-server",
                normalize_base(issuer)
            ),
        )
        .await?;
        let field = |name: &str| {
            metadata
                .get(name)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("authorization server metadata has no {name}"))
        };
        let issuer = metadata
            .get("issuer")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| issuer.to_string());
        Ok((
            issuer,
            field("authorization_endpoint")?,
            field("token_endpoint")?,
            field("pushed_authorization_request_endpoint")?,
        ))
    }

    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
        key: &DpopKey,
        nonce: Option<&str>,
    ) -> Result<(serde_json::Value, Option<String>), String> {
        let proof = key.proof("POST", url, nonce, None)?;
        let response = self
            .http
            .post(url)
            .header("DPoP", proof)
            .form(form)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let next_nonce = response
            .headers()
            .get("DPoP-Nonce")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.map_err(|error| error.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(
            |_| serde_json::json!({ "error": body.chars().take(300).collect::<String>() }),
        );
        if status == reqwest::StatusCode::BAD_REQUEST
            && value.get("error").and_then(|value| value.as_str()) == Some("use_dpop_nonce")
            && let Some(nonce) = next_nonce
        {
            return Box::pin(self.post_form(url, form, key, Some(&nonce))).await;
        }
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                value
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("authorization server refused the request")
            ));
        }
        Ok((value, next_nonce))
    }
}

async fn get_json(http: &reqwest::Client, url: &str) -> Result<serde_json::Value, String> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|error| format!("GET {url} failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("GET {url} failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "GET {url} failed with HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|error| format!("GET {url} returned invalid JSON: {error}"))
}

/// The client metadata document at the URL used as the OAuth `client_id`.
pub fn client_metadata(public_url: &str) -> serde_json::Value {
    let base = normalize_base(public_url);
    serde_json::json!({
        "client_id": format!("{base}/oauth-client-metadata.json"),
        "client_name": "atmospheric-horizon",
        "client_uri": base,
        "redirect_uris": [format!("{base}/link/callback")],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "scope": SCOPE,
        "token_endpoint_auth_method": "none",
        "application_type": "web",
        "dpop_bound_access_tokens": true
    })
}

/// A decoded JWS header/claims pair, for tests and diagnostics.
#[cfg(test)]
pub(crate) fn decode_jws(token: &str) -> (serde_json::Value, serde_json::Value) {
    let mut parts = token.split('.');
    let header = serde_json::from_slice(&b64url_decode(parts.next().unwrap()).unwrap()).unwrap();
    let claims = serde_json::from_slice(&b64url_decode(parts.next().unwrap()).unwrap()).unwrap();
    (header, claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn client_metadata_names_the_public_callback() {
        let metadata = client_metadata("https://links.example/");
        assert_eq!(
            metadata["client_id"],
            "https://links.example/oauth-client-metadata.json"
        );
        assert_eq!(
            metadata["redirect_uris"][0],
            "https://links.example/link/callback"
        );
        assert_eq!(metadata["application_type"], "web");
        assert_eq!(metadata["dpop_bound_access_tokens"], true);
    }

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

    #[test]
    fn pkce_challenge_is_deterministic_base64url() {
        let challenge = pkce_challenge("verifier");
        assert_eq!(challenge, pkce_challenge("verifier"));
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
        assert_ne!(challenge, pkce_challenge("other"));
    }

    type Captured = Arc<Mutex<Vec<(String, String, String)>>>;

    fn oauth_stub() -> (String, Captured) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let base = url.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let seen = Arc::clone(&seen);
                let base = base.clone();
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let end = loop {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos;
                        }
                        let n = match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_string();
                    let line = head.lines().next().unwrap_or_default();
                    let mut words = line.split_whitespace();
                    let method = words.next().unwrap_or_default().to_string();
                    let target = words.next().unwrap_or_default().to_string();
                    let path = target.split('?').next().unwrap_or_default().to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    let mut body = buf[end + 4..].to_vec();
                    while body.len() < length {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => body.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&body).to_string();
                    seen.lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), body.clone()));
                    let payload = match (method.as_str(), path.as_str()) {
                        ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
                            r#"{"did":"did:plc:stubuser"}"#.to_string()
                        }
                        ("GET", "/stubuser") => format!(
                            r##"{{"service":[{{"id":"#atproto_pds","serviceEndpoint":"{base}"}}]}}"##
                        ),
                        ("GET", "/.well-known/oauth-protected-resource") => format!(
                            r#"{{"authorization_servers":["{base}"]}}"#
                        ),
                        ("GET", "/.well-known/oauth-authorization-server") => format!(
                            r#"{{"issuer":"{base}","authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token","pushed_authorization_request_endpoint":"{base}/par"}}"#
                        ),
                        ("POST", "/par") => {
                            r#"{"request_uri":"urn:atmospheric:test:req1","expires_in":60}"#
                                .to_string()
                        }
                        ("POST", "/token") => {
                            r#"{"access_token":"access","token_type":"DPoP","sub":"did:plc:stubuser","scope":"atproto transition:generic"}"#
                                .to_string()
                        }
                        _ => r#"{"error":"not found"}"#.to_string(),
                    };
                    let status = if payload.contains("\"error\"") {
                        "404 Not Found"
                    } else {
                        "200 OK"
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        (url, requests)
    }

    #[test]
    fn browser_oauth_start_and_finish_against_a_stub() {
        let (base, requests) = oauth_stub();
        let client = OAuthClient::new(
            "https://links.example",
            Resolver {
                handle_service: base.clone(),
                plc_directory: base.clone(),
            },
        );
        let started = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(client.start("Stub.Test"))
            .unwrap();
        assert!(
            started
                .authorize_url
                .starts_with(&format!("{base}/authorize?"))
        );
        assert!(
            started
                .authorize_url
                .contains("request_uri=urn%3Aatmospheric%3Atest%3Areq1")
        );
        assert_eq!(started.pending.did, "did:plc:stubuser");
        assert_eq!(started.pending.issuer, base);

        let identity = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(client.finish(&started.pending, "code", Some(&base)))
            .unwrap();
        assert_eq!(identity.did, "did:plc:stubuser");
        assert_eq!(identity.handle, "stub.test");

        let requests = requests.lock().unwrap();
        let par = requests
            .iter()
            .find(|(method, path, _)| method == "POST" && path == "/par")
            .unwrap();
        assert!(par.2.contains("code_challenge_method=S256"));
        assert!(par.2.contains("dpop_jkt="));
        assert!(
            par.2
                .contains("client_id=https%3A%2F%2Flinks.example%2Foauth-client-metadata.json")
        );
        let token = requests
            .iter()
            .find(|(method, path, _)| method == "POST" && path == "/token")
            .unwrap();
        assert!(token.2.contains("grant_type=authorization_code"));
        assert!(token.2.contains("code_verifier="));
    }

    #[test]
    fn browser_oauth_rejects_a_mismatched_issuer() {
        let (base, _) = oauth_stub();
        let client = OAuthClient::new(
            "https://links.example",
            Resolver {
                handle_service: base.clone(),
                plc_directory: base.clone(),
            },
        );
        let started = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(client.start("stub.test"))
            .unwrap();
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(client.finish(&started.pending, "code", Some("https://other.example")))
            .unwrap_err();
        assert!(error.contains("does not match"));
    }
}
