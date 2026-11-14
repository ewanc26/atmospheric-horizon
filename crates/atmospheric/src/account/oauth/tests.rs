//! Offline tests for the OAuth flow: pure-function unit tests plus a stub
//! authorization server/PDS (shared with the worker tests) that exercises the
//! whole PAR → redirect → token → refresh flow and verifies every DPoP proof.

use super::*;
use p256::ecdsa::signature::Verifier;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Pure-function unit tests
// ---------------------------------------------------------------------------

#[test]
fn pkce_verifier_and_challenge_follow_rfc7636() {
    let verifier = pkce_verifier();
    assert!((43..=128).contains(&verifier.len()));
    assert!(
        verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
    );
    assert_eq!(
        pkce_challenge(&verifier),
        b64url(&Sha256::digest(verifier.as_bytes()))
    );
    // A different verifier gives a different challenge.
    assert_ne!(pkce_challenge(&pkce_verifier()), pkce_challenge(&verifier));
}

#[test]
fn client_id_is_the_loopback_form_with_the_scope_declared() {
    let id = client_id(DEFAULT_SCOPE);
    // The current spec's loopback form: origin http://localhost, empty path,
    // declared scope as the `scope` query parameter (verified against the real
    // bsky.social and eurosky.social authorization servers).
    assert_eq!(id, "http://localhost?scope=atproto%20transition%3Ageneric");
    assert!(!id.contains("127.0.0.1"));
}

#[test]
fn htu_normalisation_strips_query_and_default_ports() {
    assert_eq!(
        normalize_url("https://pds.example/xrpc/com.atproto.repo.createRecord?a=b").unwrap(),
        "https://pds.example/xrpc/com.atproto.repo.createRecord"
    );
    assert_eq!(
        normalize_url("http://127.0.0.1:8080/").unwrap(),
        "http://127.0.0.1:8080/"
    );
    assert_eq!(
        normalize_url("http://host.example:80/a").unwrap(),
        "http://host.example/a"
    );
    assert_eq!(
        normalize_url("https://host.example").unwrap(),
        "https://host.example/"
    );
    assert_eq!(
        normalize_url("https://host.example:8443/x?y=z").unwrap(),
        "https://host.example:8443/x"
    );
}

#[test]
fn query_parsing_decodes_percent_encoding() {
    let params = parse_query("code=abc%2F123&state=x%20y&empty=");
    assert_eq!(params.get("code").unwrap(), "abc/123");
    assert_eq!(params.get("state").unwrap(), "x y");
    assert_eq!(params.get("empty").unwrap(), "");
    assert_eq!(url_encode("a b/c:d"), "a%20b%2Fc%3Ad");
}

#[test]
fn dpop_key_round_trips_through_the_private_jwk() {
    let key = DpopKey::generate();
    let rebuilt = DpopKey::from_jwk_json(&key.to_jwk_json()).unwrap();
    assert_eq!(key.thumbprint(), rebuilt.thumbprint());
    assert_eq!(key.public_jwk(), rebuilt.public_jwk());
    // The persisted form really is the private JWK.
    let jwk: serde_json::Value = serde_json::from_str(&key.to_jwk_json()).unwrap();
    assert!(jwk["d"].is_string());
    assert!(DpopKey::from_jwk_json(r#"{"kty":"EC","crv":"P-256"}"#).is_err());
}

#[test]
fn thumbprint_is_the_rfc7638_canonical_hash() {
    let key = DpopKey::generate();
    let jwk = key.public_jwk();
    let canonical = format!(
        "{{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"{}\",\"y\":\"{}\"}}",
        jwk["x"].as_str().unwrap(),
        jwk["y"].as_str().unwrap()
    );
    assert_eq!(
        key.thumbprint(),
        b64url(&Sha256::digest(canonical.as_bytes()))
    );
}

#[test]
fn dpop_proof_is_a_signed_es256_jwt_with_the_right_claims() {
    let key = DpopKey::generate();
    let proof = key
        .proof(
            "POST",
            "https://pds.example/xrpc/com.atproto.repo.createRecord",
            Some("nonce-1"),
            Some("ath-1"),
        )
        .unwrap();
    let parts: Vec<&str> = proof.split('.').collect();
    assert_eq!(parts.len(), 3);

    let header: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["typ"], "dpop+jwt");
    assert_eq!(header["alg"], "ES256");
    assert_eq!(
        header["jwk"],
        key.public_jwk(),
        "the header carries the public JWK"
    );

    let claims: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["htm"], "POST");
    assert_eq!(
        claims["htu"],
        "https://pds.example/xrpc/com.atproto.repo.createRecord"
    );
    assert_eq!(claims["nonce"], "nonce-1");
    assert_eq!(claims["ath"], "ath-1");
    assert!(claims["jti"].as_str().is_some_and(|j| !j.is_empty()));
    assert!(claims["iat"].is_u64());

    // The ES256 signature (raw R||S) verifies against the header's JWK.
    let jwk = &header["jwk"];
    let mut point_bytes = vec![0x04u8];
    point_bytes.extend(b64url_decode(jwk["x"].as_str().unwrap()).unwrap());
    point_bytes.extend(b64url_decode(jwk["y"].as_str().unwrap()).unwrap());
    let point = p256::EncodedPoint::from_bytes(&point_bytes).unwrap();
    let verifying = p256::ecdsa::VerifyingKey::from_encoded_point(&point).unwrap();
    let signature = Signature::try_from(b64url_decode(parts[2]).unwrap().as_slice()).unwrap();
    verifying
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();

    // Without optional claims they are absent.
    let proof = key
        .proof("GET", "https://pds.example/", None, None)
        .unwrap();
    let claims: serde_json::Value =
        serde_json::from_slice(&b64url_decode(proof.split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert!(claims.get("nonce").is_none());
    assert!(claims.get("ath").is_none());
}

#[test]
fn access_token_hash_is_b64url_sha256_of_the_token() {
    assert_eq!(
        access_token_hash("token-abc"),
        b64url(&Sha256::digest(b"token-abc"))
    );
}

// ---------------------------------------------------------------------------
// The stub authorization server + PDS + resolver
// ---------------------------------------------------------------------------

/// One captured HTTP request.
#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub method: String,
    pub path: String,
    pub body: String,
    pub authorization: Option<String>,
    pub dpop: Option<String>,
}

/// Verify a DPoP proof JWT: signature against the header JWK, plus htm/htu and
/// optionally the `ath` claim. Returns the header for further assertions.
pub(crate) fn verify_proof(
    proof: &str,
    expected_method: &str,
    expected_htu: &str,
    expected_ath: Option<&str>,
) -> serde_json::Value {
    let parts: Vec<&str> = proof.split('.').collect();
    assert_eq!(parts.len(), 3, "a proof is three base64url segments");
    let header: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["typ"], "dpop+jwt");
    assert_eq!(header["alg"], "ES256");
    let claims: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["htm"], expected_method, "htm claim");
    assert_eq!(claims["htu"], expected_htu, "htu claim");
    match expected_ath {
        Some(ath) => assert_eq!(claims["ath"], ath, "ath claim"),
        None => assert!(claims.get("ath").is_none(), "no ath without a token"),
    }
    let jwk = &header["jwk"];
    let mut point_bytes = vec![0x04u8];
    point_bytes.extend(b64url_decode(jwk["x"].as_str().unwrap()).unwrap());
    point_bytes.extend(b64url_decode(jwk["y"].as_str().unwrap()).unwrap());
    let point = p256::EncodedPoint::from_bytes(&point_bytes).unwrap();
    let verifying = p256::ecdsa::VerifyingKey::from_encoded_point(&point).unwrap();
    let signature = Signature::try_from(b64url_decode(parts[2]).unwrap().as_slice()).unwrap();
    verifying
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the DPoP proof signature verifies");
    header
}

/// The thumbprint (dpop_jkt) of the JWK inside a proof header.
fn jkt_of(header: &serde_json::Value) -> String {
    let jwk = &header["jwk"];
    let canonical = format!(
        "{{\"crv\":\"{}\",\"kty\":\"{}\",\"x\":\"{}\",\"y\":\"{}\"}}",
        jwk["crv"].as_str().unwrap_or_default(),
        jwk["kty"].as_str().unwrap_or_default(),
        jwk["x"].as_str().unwrap_or_default(),
        jwk["y"].as_str().unwrap_or_default(),
    );
    b64url(&Sha256::digest(canonical.as_bytes()))
}

/// A stub that plays handle resolver, PLC directory, PDS and authorization
/// server on one loopback HTTP server. `wrong_state` makes `/authorize`
/// redirect with a mismatched state.
pub(crate) struct StubAuthServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Captured>>>,
    /// The currently valid OAuth access token (rotates on refresh).
    pub access_token: Arc<Mutex<String>>,
    /// Pushed authorization requests, by request_uri.
    pub pending: Arc<Mutex<std::collections::HashMap<String, PendingPar>>>,
    shutdown: Arc<Mutex<bool>>,
}

impl StubAuthServer {
    pub fn start(wrong_state: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let access_token = Arc::new(Mutex::new(String::from("stub-oauth-access-1")));
        let pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let shutdown = Arc::new(Mutex::new(false));
        {
            let requests = Arc::clone(&requests);
            let access_token = Arc::clone(&access_token);
            let pending = Arc::clone(&pending);
            let shutdown = Arc::clone(&shutdown);
            let url = url.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if *shutdown.lock().unwrap() {
                        break;
                    }
                    let Ok(mut stream) = stream else { break };
                    let requests = Arc::clone(&requests);
                    let access_token = Arc::clone(&access_token);
                    let pending = Arc::clone(&pending);
                    let url = url.clone();
                    std::thread::spawn(move || {
                        handle(
                            &mut stream,
                            &url,
                            &requests,
                            &access_token,
                            &pending,
                            wrong_state,
                        );
                    });
                }
            });
        }
        Self {
            url,
            requests,
            access_token,
            pending,
            shutdown,
        }
    }

    /// A resolver pointing all three resolution steps at this stub.
    pub fn resolver(&self) -> Resolver {
        Resolver {
            handle_service: self.url.clone(),
            plc_directory: self.url.clone(),
        }
    }

    pub fn requests(&self) -> Vec<Captured> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for StubAuthServer {
    fn drop(&mut self) {
        *self.shutdown.lock().unwrap() = true;
    }
}

/// One pending PAR request.
#[derive(Clone)]
pub(crate) struct PendingPar {
    state: String,
    redirect_uri: String,
    code_challenge: String,
    scope: String,
}

fn handle(
    stream: &mut std::net::TcpStream,
    server_url: &str,
    requests: &Arc<Mutex<Vec<Captured>>>,
    access_token: &Arc<Mutex<String>>,
    pending: &Arc<Mutex<std::collections::HashMap<String, PendingPar>>>,
    wrong_state: bool,
) {
    use std::time::Duration;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.len() > 1 << 20 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    let mut content_length = 0usize;
    let mut authorization = None;
    let mut dpop = None;
    for line in lines {
        // Lowercase only the header *name*: values like the DPoP proof are
        // base64url and case-sensitive.
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.trim().parse().unwrap_or(0),
            "authorization" => authorization = Some(value.trim().to_string()),
            "dpop" => dpop = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let body_string = String::from_utf8_lossy(&body).to_string();
    let path = target.split('?').next().unwrap_or("/").to_string();
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    requests.lock().unwrap().push(Captured {
        method: method.clone(),
        path: target.clone(),
        body: body_string.clone(),
        authorization: authorization.clone(),
        dpop: dpop.clone(),
    });

    let respond = |stream: &mut std::net::TcpStream,
                   status: &str,
                   payload: String,
                   extra: &[(&str, &str)]| {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            payload.len()
        );
        for (name, value) in extra {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(&payload);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    };

    let form = parse_query(&body_string);
    let _ = query;

    match (method.as_str(), path.as_str()) {
        ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
            respond(
                stream,
                "200 OK",
                r#"{"did":"did:plc:stubuser"}"#.to_string(),
                &[],
            );
        }
        ("GET", "/did:plc:stubuser") => {
            let doc = serde_json::json!({
                "@context": ["https://www.w3.org/ns/did/v1"],
                "id": "did:plc:stubuser",
                "alsoKnownAs": ["at://stub.test"],
                "service": [{
                    "id": "#atproto_pds",
                    "type": "AtprotoPersonalDataServer",
                    "serviceEndpoint": server_url,
                }],
            })
            .to_string();
            respond(stream, "200 OK", doc, &[]);
        }
        ("GET", "/.well-known/oauth-protected-resource") => {
            respond(
                stream,
                "200 OK",
                format!(r#"{{"authorization_servers":["{server_url}"]}}"#),
                &[],
            );
        }
        ("GET", "/.well-known/oauth-authorization-server") => {
            respond(
                stream,
                "200 OK",
                format!(
                    r#"{{"issuer":"{server_url}","authorization_endpoint":"{server_url}/authorize","token_endpoint":"{server_url}/token","pushed_authorization_request_endpoint":"{server_url}/par","scopes_supported":["atproto","transition:generic"]}}"#
                ),
                &[],
            );
        }
        ("POST", "/par") => {
            let proof = dpop.clone().expect("PAR carries a DPoP proof");
            let header = verify_proof(&proof, "POST", &format!("{server_url}/par"), None);
            let jkt = jkt_of(&header);
            assert_eq!(
                form.get("dpop_jkt"),
                Some(&jkt),
                "dpop_jkt matches the proof key's thumbprint"
            );
            assert_eq!(
                form.get("code_challenge_method").map(String::as_str),
                Some("S256")
            );
            assert_eq!(form.get("response_type").map(String::as_str), Some("code"));
            let claims: serde_json::Value =
                serde_json::from_slice(&b64url_decode(proof.split('.').nth(1).unwrap()).unwrap())
                    .unwrap();
            if claims.get("nonce").is_none() {
                // First attempt: challenge with a nonce (RFC 9449 §7).
                respond(
                    stream,
                    "400 Bad Request",
                    r#"{"error":"use_dpop_nonce"}"#.to_string(),
                    &[("DPoP-Nonce", "stub-nonce-1")],
                );
                return;
            }
            assert_eq!(claims["nonce"], "stub-nonce-1");
            pending.lock().unwrap().insert(
                "urn:atmospheric:test:req1".to_string(),
                PendingPar {
                    state: form.get("state").cloned().unwrap_or_default(),
                    redirect_uri: form.get("redirect_uri").cloned().unwrap_or_default(),
                    code_challenge: form.get("code_challenge").cloned().unwrap_or_default(),
                    scope: form.get("scope").cloned().unwrap_or_default(),
                },
            );
            respond(
                stream,
                "201 Created",
                r#"{"request_uri":"urn:atmospheric:test:req1","expires_in":60}"#.to_string(),
                &[],
            );
        }
        ("GET", "/authorize") => {
            let par = pending
                .lock()
                .unwrap()
                .get("urn:atmospheric:test:req1")
                .cloned();
            let Some(par) = par else {
                respond(
                    stream,
                    "400 Bad Request",
                    r#"{"error":"invalid_request_uri"}"#.to_string(),
                    &[],
                );
                return;
            };
            let state = if wrong_state {
                "wrong-state".to_string()
            } else {
                par.state
            };
            let location = format!(
                "{}?code=stub-code&state={}",
                par.redirect_uri,
                url_encode(&state)
            );
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
        ("POST", "/token") => {
            let proof = dpop
                .clone()
                .expect("the token request carries a DPoP proof");
            verify_proof(&proof, "POST", &format!("{server_url}/token"), None);
            match form.get("grant_type").map(String::as_str) {
                Some("authorization_code") => {
                    assert_eq!(form.get("code").map(String::as_str), Some("stub-code"));
                    let par = pending
                        .lock()
                        .unwrap()
                        .get("urn:atmospheric:test:req1")
                        .cloned()
                        .unwrap();
                    assert_eq!(
                        form.get("code_verifier").map(|v| pkce_challenge(v)),
                        Some(par.code_challenge.clone()),
                        "the PKCE verifier matches the pushed challenge"
                    );
                    assert!(
                        form.get("client_id")
                            .unwrap()
                            .starts_with("http://localhost?")
                    );
                    *access_token.lock().unwrap() = "stub-oauth-access-1".into();
                    respond(
                        stream,
                        "200 OK",
                        format!(
                            r#"{{"access_token":"stub-oauth-access-1","token_type":"DPoP","refresh_token":"stub-oauth-refresh-1","expires_in":3600,"sub":"did:plc:stubuser","scope":"{}"}}"#,
                            par.scope.replace('"', "")
                        ),
                        &[],
                    );
                }
                Some("refresh_token") => {
                    assert_eq!(
                        form.get("refresh_token").map(String::as_str),
                        Some("stub-oauth-refresh-1")
                    );
                    *access_token.lock().unwrap() = "stub-oauth-access-2".into();
                    respond(
                        stream,
                        "200 OK",
                        r#"{"access_token":"stub-oauth-access-2","token_type":"DPoP","refresh_token":"stub-oauth-refresh-2","expires_in":30,"sub":"did:plc:stubuser","scope":"atproto transition:generic"}"#.to_string(),
                        &[],
                    );
                }
                other => {
                    let _ = other;
                    respond(
                        stream,
                        "400 Bad Request",
                        r#"{"error":"unsupported_grant_type"}"#.to_string(),
                        &[],
                    );
                }
            }
        }
        ("GET", "/xrpc/com.atproto.server.getSession") => {
            let access = access_token.lock().unwrap().clone();
            let expected_auth = format!("DPoP {access}");
            assert_eq!(authorization.as_deref(), Some(expected_auth.as_str()));
            let proof = dpop.clone().expect("getSession carries a DPoP proof");
            verify_proof(
                &proof,
                "GET",
                &format!("{server_url}/xrpc/com.atproto.server.getSession"),
                Some(&access_token_hash(&access)),
            );
            respond(
                stream,
                "200 OK",
                r#"{"did":"did:plc:stubuser","handle":"stub.test","active":true}"#.to_string(),
                &[],
            );
        }
        ("GET", "/xrpc/com.atproto.repo.getRecord") => {
            // The server-profile writer reads the existing record first; a
            // missing one is RecordNotFound, which it treats as an upsert.
            respond(
                stream,
                "400 Bad Request",
                r#"{"error":"RecordNotFound","message":"no record"}"#.to_string(),
                &[],
            );
        }
        ("POST", "/xrpc/com.atproto.repo.putRecord") => {
            let access = access_token.lock().unwrap().clone();
            let expected_auth = format!("DPoP {access}");
            assert_eq!(authorization.as_deref(), Some(expected_auth.as_str()));
            let proof = dpop.clone().expect("putRecord carries a DPoP proof");
            verify_proof(
                &proof,
                "POST",
                &format!("{server_url}/xrpc/com.atproto.repo.putRecord"),
                Some(&access_token_hash(&access)),
            );
            respond(
                stream,
                "200 OK",
                r#"{"uri":"at://did:plc:stubuser/at.atpcraft.server.profile/self","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}"#.to_string(),
                &[],
            );
        }
        ("POST", "/xrpc/com.atproto.repo.createRecord") => {
            let access = access_token.lock().unwrap().clone();
            let expected_auth = format!("DPoP {access}");
            assert_eq!(authorization.as_deref(), Some(expected_auth.as_str()));
            let proof = dpop.clone().expect("createRecord carries a DPoP proof");
            verify_proof(
                &proof,
                "POST",
                &format!("{server_url}/xrpc/com.atproto.repo.createRecord"),
                Some(&access_token_hash(&access)),
            );
            respond(
                stream,
                "200 OK",
                r#"{"uri":"at://did:plc:stubuser/app.bsky.feed.post/stubrec","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}"#.to_string(),
                &[],
            );
        }
        _ => {
            respond(
                stream,
                "404 Not Found",
                r#"{"error":"UnknownEndpoint"}"#.to_string(),
                &[],
            );
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers for simulating the browser (std, so worker tests can use them)
// ---------------------------------------------------------------------------

/// A raw `GET` with no redirect following: returns (status line, headers, body).
pub(crate) fn http_get_raw(url: &str) -> (String, Vec<(String, String)>, String) {
    let uri: atrium_xrpc::http::Uri = url.parse().unwrap();
    let host = uri.host().unwrap().to_string();
    let port = uri.port_u16().unwrap_or(80);
    let path = {
        let p = uri.path();
        if let Some(q) = uri.query() {
            format!("{p}?{q}")
        } else if p.is_empty() {
            "/".to_string()
        } else {
            p.to_string()
        }
    };
    let mut stream = std::net::TcpStream::connect((host.as_str(), port)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let mut lines = head.lines();
    let status = lines.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    (status, headers, body)
}

/// Simulate the browser for an authorize URL: GET it (no redirect follow),
/// then GET the 302 Location (the loopback redirect). Returns the loopback
/// page's status line, so tests can assert 200 (good state) or 400 (mismatch).
pub(crate) fn follow_authorize_redirect(authorize_url: &str) -> String {
    let (status, headers, _body) = http_get_raw(authorize_url);
    assert!(status.contains("302"), "authorize redirects, got: {status}");
    let location = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.clone())
        .expect("a Location header");
    let (status, _headers, _body) = http_get_raw(&location);
    status
}

// ---------------------------------------------------------------------------
// Flow tests against the stub
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_oauth_flow_against_a_stub_authorization_server() {
    let stub = StubAuthServer::start(false);
    let http = reqwest::Client::new();
    let resolver = stub.resolver();

    let mut authorization = start_authorization(
        &http,
        &resolver,
        "stub.test",
        &client_id(DEFAULT_SCOPE),
        DEFAULT_SCOPE,
    )
    .await
    .unwrap();
    assert_eq!(authorization.did, "did:plc:stubuser");
    assert_eq!(authorization.pds_url, stub.url);
    assert!(authorization.redirect_uri.starts_with("http://127.0.0.1:"));
    assert!(
        authorization
            .authorize_url
            .starts_with(&format!("{}/authorize?", stub.url))
    );
    assert!(
        authorization
            .authorize_url
            .contains("request_uri=urn%3Aatmospheric%3Atest%3Areq1")
    );

    // The browser follows the authorize URL; the redirect lands on our listener.
    let authorize_url = authorization.authorize_url.clone();
    let browser = std::thread::spawn(move || follow_authorize_redirect(&authorize_url));
    let code = wait_for_redirect(&mut authorization, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(code, "stub-code");
    let loopback_status = browser.join().unwrap();
    assert!(
        loopback_status.contains("200"),
        "the loopback page answers 200: {loopback_status}"
    );

    let tokens = exchange_code(&http, &authorization, &code).await.unwrap();
    assert_eq!(tokens.access_token, "stub-oauth-access-1");
    assert_eq!(
        tokens.refresh_token.as_deref(),
        Some("stub-oauth-refresh-1")
    );
    assert_eq!(tokens.sub, "did:plc:stubuser");
    assert_eq!(tokens.scope.as_deref(), Some(DEFAULT_SCOPE));

    // The PAR was challenged once with a nonce, then accepted with it.
    let par_requests: Vec<_> = stub
        .requests()
        .into_iter()
        .filter(|r| r.path == "/par")
        .collect();
    assert_eq!(par_requests.len(), 2, "nonce challenge then accepted PAR");
    assert!(par_requests.iter().all(|r| r.method == "POST"));
    // The accepted PAR is the pending one, with the pushed challenge and scope.
    {
        let pending = stub.pending.lock().unwrap();
        let par = pending.get("urn:atmospheric:test:req1").unwrap();
        assert_eq!(par.scope, DEFAULT_SCOPE);
        assert_eq!(par.redirect_uri, authorization.redirect_uri);
        assert_eq!(
            pkce_challenge(&authorization.code_verifier),
            par.code_challenge
        );
    }

    // Refresh rotates the token at the token endpoint.
    let refreshed = refresh(
        &http,
        &authorization.token_endpoint,
        &authorization.client_id,
        tokens.refresh_token.as_deref().unwrap(),
        &authorization.key,
    )
    .await
    .unwrap();
    assert_eq!(refreshed.access_token, "stub-oauth-access-2");
    assert_eq!(
        refreshed.refresh_token.as_deref(),
        Some("stub-oauth-refresh-2")
    );
    assert_eq!(refreshed.expires_in, Some(30));
    assert_eq!(*stub.access_token.lock().unwrap(), "stub-oauth-access-2");
}

#[tokio::test]
async fn redirect_with_a_wrong_state_is_rejected() {
    let stub = StubAuthServer::start(true); // wrong_state
    let http = reqwest::Client::new();
    let resolver = stub.resolver();
    let mut authorization = start_authorization(
        &http,
        &resolver,
        "stub.test",
        &client_id(DEFAULT_SCOPE),
        DEFAULT_SCOPE,
    )
    .await
    .unwrap();
    let authorize_url = authorization.authorize_url.clone();
    let browser = std::thread::spawn(move || follow_authorize_redirect(&authorize_url));
    let error = wait_for_redirect(&mut authorization, Duration::from_secs(10))
        .await
        .expect_err("a mismatched state must fail");
    assert!(error.to_string().contains("state"), "{error}");
    let loopback_status = browser.join().unwrap();
    assert!(
        loopback_status.contains("400"),
        "the loopback page rejects the bad state: {loopback_status}"
    );
}

#[test]
fn colibri_player_scope_has_message_grants_but_only_the_server_scope_has_the_stream() {
    let player = scope_for_colibri(true, "did:web:api.colibri.social");
    assert!(player.contains("repo:social.colibri.message?action=create&action=delete"));
    assert!(player.contains("repo:social.colibri.membership?action=create&action=delete"));
    assert!(!player.contains("rpc:social.colibri.sync.subscribeEvents"));
    assert_eq!(
        scope_for_colibri(false, "did:web:api.colibri.social"),
        DEFAULT_SCOPE
    );

    let server = scope_for_server(DEFAULT_SCOPE, true, "did:web:api.colibri.social", false);
    assert!(server.contains(
        "rpc:social.colibri.sync.subscribeEvents?aud=did:web:api.colibri.social#colibri_appview"
    ));
    // The broad default base already covers the descriptor write.
    assert_eq!(
        scope_for_server(DEFAULT_SCOPE, false, "did:web:api.colibri.social", false),
        DEFAULT_SCOPE
    );
}

#[test]
fn scope_for_falls_back_to_the_default_base_and_trims_a_configured_one() {
    assert_eq!(
        scope_for("", false, "did:web:api.colibri.social"),
        DEFAULT_SCOPE
    );
    assert_eq!(
        scope_for("   ", false, "did:web:api.colibri.social"),
        DEFAULT_SCOPE
    );
}

#[test]
fn a_custom_base_gets_the_required_player_grants_once() {
    let base = "atproto repo:app.bsky.feed.post?action=create";
    let scope = scope_for(base, false, "did:web:api.colibri.social");
    assert!(scope.starts_with(base));
    for grant in PLAYER_GRANTS {
        assert_eq!(
            scope
                .split_ascii_whitespace()
                .filter(|token| *token == grant)
                .count(),
            1,
            "{grant} appears once"
        );
    }
    // Rebuilding from the result changes nothing.
    assert_eq!(
        scope_for(&scope, false, "did:web:api.colibri.social"),
        scope
    );

    // A trimmed custom base is the prefix.
    let trimmed = scope_for("  atproto  ", false, "did:web:api.colibri.social");
    assert!(trimmed.starts_with("atproto repo:at.atpcraft.player.snapshot"));

    // The bridge adds each Colibri grant once, and only the server gets the stream.
    let player = scope_for(base, true, "did:web:api.colibri.social");
    assert_eq!(
        player
            .matches("repo:social.colibri.message?action=create&action=delete")
            .count(),
        1
    );
    assert_eq!(
        player
            .matches("repo:social.colibri.membership?action=create&action=delete")
            .count(),
        1
    );
    assert!(!player.contains("rpc:social.colibri.sync.subscribeEvents"));
    let server = scope_for_server(base, true, "did:web:api.colibri.social", false);
    assert_eq!(
        server
            .matches(
                "rpc:social.colibri.sync.subscribeEvents?aud=did:web:api.colibri.social#colibri_appview"
            )
            .count(),
        1
    );
    assert_eq!(
        scope_for_server(&server, true, "did:web:api.colibri.social", false),
        server
    );
}

#[test]
fn the_server_scope_adds_its_grants_and_a_broad_base_needs_none() {
    let base = "atproto";
    let player = scope_for(base, false, "did:web:api.colibri.social");
    assert!(!player.contains(SERVER_PROFILE_GRANT));
    let server = scope_for_server(base, false, "did:web:api.colibri.social", false);
    assert_eq!(server.matches(SERVER_PROFILE_GRANT).count(), 1);
    assert_eq!(
        scope_for_server(&server, false, "did:web:api.colibri.social", false),
        server
    );

    // `transition:generic` already covers the writes, so the default string
    // stays exactly as before.
    assert_eq!(
        scope_for(DEFAULT_SCOPE, false, "did:web:api.colibri.social"),
        DEFAULT_SCOPE
    );
    assert_eq!(
        scope_for_server(DEFAULT_SCOPE, false, "did:web:api.colibri.social", false),
        DEFAULT_SCOPE
    );

    // Enabling the labeler adds only its declaration grant to a narrow scope.
    let labeler = scope_for_server(base, false, "did:web:api.colibri.social", true);
    assert_eq!(labeler.matches(LABELER_SERVICE_GRANT).count(), 1);
    assert_eq!(
        scope_for_server(&labeler, false, "did:web:api.colibri.social", true),
        labeler
    );
    assert_eq!(
        scope_for_server(DEFAULT_SCOPE, false, "did:web:api.colibri.social", true),
        DEFAULT_SCOPE,
        "the broad scope already covers the declaration write"
    );
}

#[tokio::test]
async fn redirect_wait_times_out_without_a_browser() {
    let stub = StubAuthServer::start(false);
    let http = reqwest::Client::new();
    let resolver = stub.resolver();
    let mut authorization = start_authorization(
        &http,
        &resolver,
        "stub.test",
        &client_id(DEFAULT_SCOPE),
        DEFAULT_SCOPE,
    )
    .await
    .unwrap();
    let error = wait_for_redirect(&mut authorization, Duration::from_millis(300))
        .await
        .expect_err("no redirect arrives");
    assert!(error.to_string().contains("timed out"), "{error}");
}
