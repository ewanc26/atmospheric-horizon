//! Full browser-link OAuth flow through the real router.
//!
//! The stub serves handle resolution, the DID document, the PDS protected
//! resource, authorization-server metadata, PAR and the token endpoint. It
//! verifies the PKCE verifier against the PAR challenge, so the callback only
//! succeeds when the router carried the one-time state and verifier through.

use atmospheric_server::{ServerConfig, app, oauth::Resolver, store::LinkStore};
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tower::ServiceExt;

const UUID: &str = "00000000-0000-4000-8000-0000000000e2";
const LINK_TOKEN: &str = "test-link-token";

#[derive(Default)]
struct Captured {
    par_form: Option<String>,
    token_form: Option<String>,
    dpop: Vec<String>,
}

fn temp_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir()
        .join(format!(
            "atmospheric-oauth-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("links.json")
}

fn config(resolver: Resolver) -> ServerConfig {
    ServerConfig {
        admin_token: Some("test-admin".into()),
        link_token: Some(LINK_TOKEN.into()),
        public_url: Some("https://links.example".into()),
        oauth_resolver: Some(resolver),
    }
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn form_value(form: &str, key: &str) -> Option<String> {
    form.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name == key {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn start_stub() -> (String, Arc<Mutex<Captured>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Captured::default()));
    let base = url.clone();
    let seen = Arc::clone(&captured);
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
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let mut lines = head.lines();
                let line = lines.next().unwrap_or_default();
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or_default().to_string();
                let target = words.next().unwrap_or_default().to_string();
                let path = target.split('?').next().unwrap_or_default().to_string();
                let length = lines
                    .clone()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let dpop = lines.find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("dpop:")
                        .map(|value| value.trim().to_owned())
                });
                let mut body = buf[end + 4..].to_vec();
                while body.len() < length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                let body = String::from_utf8_lossy(&body).to_string();
                if let Some(dpop) = dpop {
                    seen.lock().unwrap().dpop.push(dpop);
                }

                let (status, payload) = match (method.as_str(), path.as_str()) {
                    ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
                        ("200 OK", r#"{"did":"did:plc:e2e"}"#.to_string())
                    }
                    ("GET", "/e2e") => (
                        "200 OK",
                        format!(
                            r##"{{"service":[{{"id":"#atproto_pds","serviceEndpoint":"{base}"}}]}}"##
                        ),
                    ),
                    ("GET", "/.well-known/oauth-protected-resource") => (
                        "200 OK",
                        format!(r#"{{"authorization_servers":["{base}"]}}"#),
                    ),
                    ("GET", "/.well-known/oauth-authorization-server") => (
                        "200 OK",
                        format!(
                            r#"{{"issuer":"{base}","authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token","pushed_authorization_request_endpoint":"{base}/par"}}"#
                        ),
                    ),
                    ("POST", "/par") => {
                        let ok = form_value(&body, "client_id").as_deref()
                            == Some("https://links.example/oauth-client-metadata.json")
                            && form_value(&body, "code_challenge_method").as_deref()
                                == Some("S256")
                            && form_value(&body, "dpop_jkt").is_some();
                        if ok {
                            seen.lock().unwrap().par_form = Some(body.clone());
                            (
                                "200 OK",
                                r#"{"request_uri":"urn:atmospheric:test:req1","expires_in":60}"#
                                    .to_string(),
                            )
                        } else {
                            (
                                "400 Bad Request",
                                r#"{"error":"invalid_request"}"#.to_string(),
                            )
                        }
                    }
                    ("POST", "/token") => {
                        let par = seen.lock().unwrap().par_form.clone().unwrap_or_default();
                        let expected = form_value(&par, "code_challenge");
                        let verifier = form_value(&body, "code_verifier");
                        let ok = form_value(&body, "grant_type").as_deref()
                            == Some("authorization_code")
                            && form_value(&body, "code").as_deref() == Some("test-code")
                            && verifier.as_deref().is_some_and(|verifier| {
                                expected.as_deref() == Some(&challenge(verifier))
                            });
                        if ok {
                            seen.lock().unwrap().token_form = Some(body.clone());
                            (
                                "200 OK",
                                r#"{"access_token":"test-access","token_type":"DPoP","sub":"did:plc:e2e","scope":"atproto transition:generic"}"#
                                    .to_string(),
                            )
                        } else {
                            (
                                "400 Bad Request",
                                r#"{"error":"invalid_grant"}"#.to_string(),
                            )
                        }
                    }
                    _ => ("404 Not Found", r#"{"error":"not found"}"#.to_string()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    (url, captured)
}

async fn json_request(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn browser_link_completes_oauth_and_saves_the_link() {
    let (stub_url, captured) = start_stub();
    let path = temp_path();
    let store = LinkStore::open(&path).unwrap();
    let router = app(
        store,
        config(Resolver {
            handle_service: stub_url.clone(),
            plc_directory: stub_url.clone(),
        }),
    );

    let (status, body) = json_request(
        &router,
        "POST",
        "/v1/browser/pairings",
        LINK_TOKEN,
        serde_json::json!({ "player_uuid": UUID, "player_name": "Alex" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let url = body["url"].as_str().unwrap();
    let link_path = url.trim_start_matches("https://links.example");

    let (status, page) = request(
        &router,
        "POST",
        link_path,
        Some("application/x-www-form-urlencoded"),
        "handle=stub.test",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("Continue to Bluesky"), "{page}");

    let par = captured
        .lock()
        .unwrap()
        .par_form
        .clone()
        .expect("PAR was sent");
    let state = form_value(&par, "state").expect("PAR carried state");
    let callback = format!(
        "/link/callback?code=test-code&state={}&iss={}",
        url_encode(&state),
        url_encode(&stub_url)
    );
    let (status, page) = request(&router, "GET", &callback, None, "").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("Bluesky account linked"), "{page}");

    let (status, body) = request(&router, "GET", &format!("/v1/links/{UUID}"), None, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let link: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(link["did"], "did:plc:e2e");
    assert_eq!(link["handle"], "stub.test");
    {
        let captured = captured.lock().unwrap();
        assert!(
            captured.token_form.as_deref().is_some_and(|form| {
                form_value(form, "code_verifier").is_some()
                    && form_value(form, "redirect_uri").as_deref()
                        == Some("https://links.example/link/callback")
            }),
            "token exchange carried the PKCE verifier and redirect"
        );
        assert_eq!(
            captured.dpop.len(),
            2,
            "PAR and token requests are DPoP-bound"
        );
        assert!(
            captured
                .dpop
                .iter()
                .all(|proof| proof.split('.').count() == 3)
        );
    }

    let (status, _) = request(&router, "GET", &callback, None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "state is single-use");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
