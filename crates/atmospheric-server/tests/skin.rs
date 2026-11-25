//! End-to-end browser skin publishing (M64) through the real router.
//!
//! The stub resolves the handle and PDS, serves authorization-server metadata,
//! PAR and the token endpoint, then records the blob and record writes. The
//! test checks that the callback writes only as the linked DID and that the
//! published record carries a valid endorsement from the configured key.

use atmospheric::{
    records::{EndorsementVerdict, SkinRecord, verify_endorsements},
    signing,
};
use atmospheric_server::{
    ServerConfig, app,
    model::{LinkInput, unix_now},
    oauth::Resolver,
    skin::SkinSigner,
    store::LinkStore,
};
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tower::ServiceExt;

const UUID: &str = "00000000-0000-4000-8000-0000000000c4";
const UUID_UNLINKED: &str = "00000000-0000-4000-8000-0000000000c5";
const LINK_TOKEN: &str = "test-link-token";
const ADMIN_TOKEN: &str = "test-admin-token";
const CID: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";

fn config(resolver: Option<Resolver>, signer: Option<SkinSigner>) -> ServerConfig {
    ServerConfig {
        admin_token: Some(ADMIN_TOKEN.into()),
        link_token: Some(LINK_TOKEN.into()),
        public_url: Some("https://links.example".into()),
        oauth_resolver: resolver,
        labeler: None,
        skin_signer: signer,
        operator: None,
        continuity: None,
    }
}

fn temp_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir()
        .join(format!(
            "atmospheric-server-skin-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("links.json")
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

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn start_stub() -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let base = url.clone();
    let captured = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let captured = Arc::clone(&captured);
            let base = base.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
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
                let mut headers: Vec<(String, String)> = Vec::new();
                for line in lines {
                    if let Some((name, value)) = line.split_once(':') {
                        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
                    }
                }
                let header = |name: &str| {
                    headers
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.clone())
                };
                let length = header("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let content_type = header("content-type");
                let mut body = buf[end + 4..].to_vec();
                while body.len() < length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                captured.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    content_type,
                    body: body.clone(),
                });

                let (status, payload) = match (method.as_str(), path.as_str()) {
                    ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
                        ("200 OK", r#"{"did":"did:plc:alice"}"#.to_string())
                    }
                    ("GET", "/alice") => (
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
                        let form = String::from_utf8_lossy(&body).to_string();
                        let ok = form_value(&form, "client_id").as_deref()
                            == Some("https://links.example/oauth-client-metadata.json")
                            && form_value(&form, "redirect_uri").as_deref()
                                == Some("https://links.example/skin/callback")
                            && form_value(&form, "code_challenge_method").as_deref()
                                == Some("S256")
                            && form_value(&form, "dpop_jkt").is_some();
                        if ok {
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
                        let form = String::from_utf8_lossy(&body).to_string();
                        let par = captured
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|request| request.path == "/par")
                            .map(|request| String::from_utf8_lossy(&request.body).to_string())
                            .unwrap_or_default();
                        let expected = form_value(&par, "code_challenge");
                        let verifier = form_value(&form, "code_verifier");
                        let ok = form_value(&form, "grant_type").as_deref()
                            == Some("authorization_code")
                            && form_value(&form, "code").as_deref() == Some("test-code")
                            && form_value(&form, "redirect_uri").as_deref()
                                == Some("https://links.example/skin/callback")
                            && verifier.as_deref().is_some_and(|verifier| {
                                expected.as_deref() == Some(&challenge(verifier))
                            });
                        if ok {
                            (
                                "200 OK",
                                r#"{"access_token":"test-access","token_type":"DPoP","sub":"did:plc:alice","scope":"atproto transition:generic"}"#
                                    .to_string(),
                            )
                        } else {
                            (
                                "400 Bad Request",
                                r#"{"error":"invalid_grant"}"#.to_string(),
                            )
                        }
                    }
                    ("POST", "/xrpc/com.atproto.repo.uploadBlob") => (
                        "200 OK",
                        format!(
                            r#"{{"blob":{{"$type":"blob","ref":{{"$link":"{CID}"}},"mimeType":"image/png","size":{}}}}}"#,
                            body.len()
                        ),
                    ),
                    ("GET", "/xrpc/com.atproto.repo.getRecord") => (
                        "400 Bad Request",
                        r#"{"error":"RecordNotFound","message":"Could not locate record"}"#
                            .to_string(),
                    ),
                    ("POST", "/xrpc/com.atproto.repo.putRecord") => {
                        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        let rkey = request["rkey"].as_str().unwrap_or("self");
                        (
                            "200 OK",
                            format!(
                                r#"{{"uri":"at://did:plc:alice/at.atpcraft.player.skin/{rkey}","cid":"{CID}"}}"#
                            ),
                        )
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
    (url, seen)
}

async fn json_request(
    router: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).unwrap())
    } else {
        Body::empty()
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn raw_request(
    router: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn multipart_body(boundary: &str, png: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in [("handle", "alice.test"), ("model", "slim")] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"skin\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

async fn linked_store(tag: &str) -> (LinkStore, PathBuf) {
    let path = temp_path(tag);
    let store = LinkStore::open(&path).unwrap();
    store
        .upsert_link(
            UUID,
            &LinkInput {
                player_name: "Alex".into(),
                did: "did:plc:alice".into(),
                handle: Some("alice.test".into()),
            },
            unix_now(),
        )
        .await
        .unwrap();
    (store, path)
}

#[tokio::test]
async fn skin_uploads_need_the_link_token_a_link_and_a_signer() {
    let (store, path) = linked_store("routes").await;
    let router = app(
        store,
        config(None, Some(SkinSigner::new(signing::generate()))),
    );

    let body = json!({ "player_uuid": UUID, "player_name": "Alex" });
    let (status, _) = json_request(
        &router,
        "POST",
        "/v1/browser/skins",
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = json_request(
        &router,
        "POST",
        "/v1/browser/skins",
        Some("wrong"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, response) = json_request(
        &router,
        "POST",
        "/v1/browser/skins",
        Some(LINK_TOKEN),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let url = response["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("https://links.example/skin/"));
    assert!(response["expires_at"].as_u64().unwrap() > 0);
    let skin_path = url.trim_start_matches("https://links.example");

    let (status, page) = raw_request(&router, "GET", skin_path, None, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Upload your skin"), "{page}");
    assert!(page.contains("Alex"), "{page}");
    assert!(page.contains("alice.test"), "{page}");

    let boundary = "atmospheric-test-boundary";
    let (status, page) = raw_request(
        &router,
        "POST",
        skin_path,
        Some(&format!("multipart/form-data; boundary={boundary}")),
        multipart_body(boundary, b"not a png"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("Only PNG files are accepted."), "{page}");

    let (status, _) = raw_request(&router, "GET", "/skin/unknown", None, Vec::new()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, response) = json_request(
        &router,
        "POST",
        "/v1/browser/skins",
        Some(LINK_TOKEN),
        Some(json!({ "player_uuid": UUID_UNLINKED, "player_name": "Bob" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(response["error"].as_str().unwrap().contains("link"));

    let (status, metadata) =
        json_request(&router, "GET", "/oauth-client-metadata.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let callbacks: Vec<&str> = metadata["redirect_uris"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(callbacks.contains(&"https://links.example/link/callback"));
    assert!(callbacks.contains(&"https://links.example/skin/callback"));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());

    let path = temp_path("disabled");
    let store = LinkStore::open(&path).unwrap();
    store
        .upsert_link(
            UUID,
            &LinkInput {
                player_name: "Alex".into(),
                did: "did:plc:alice".into(),
                handle: None,
            },
            unix_now(),
        )
        .await
        .unwrap();
    let (status, response) = json_request(
        &app(store, config(None, None)),
        "POST",
        "/v1/browser/skins",
        Some(LINK_TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // 5xx bodies are deliberately generic; the operator hint is logged.
    assert_eq!(response["error"], "internal server error");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn browser_skin_upload_publishes_a_signed_record() {
    let (stub_url, seen) = start_stub();
    let (store, path) = linked_store("publish").await;
    let key = signing::generate();
    let signer = SkinSigner::new(key.clone());
    let router = app(
        store,
        config(
            Some(Resolver {
                handle_service: stub_url.clone(),
                plc_directory: stub_url.clone(),
            }),
            Some(signer.clone()),
        ),
    );

    let (status, response) = json_request(
        &router,
        "POST",
        "/v1/browser/skins",
        Some(LINK_TOKEN),
        Some(json!({ "player_uuid": UUID, "player_name": "Alex", "ttl_seconds": 300 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let skin_path = response["url"]
        .as_str()
        .unwrap()
        .trim_start_matches("https://links.example")
        .to_owned();

    let png = b"\x89PNG\r\n\x1a\nbrowser-skin".to_vec();
    let boundary = "atmospheric-test-boundary";
    let (status, page) = raw_request(
        &router,
        "POST",
        &skin_path,
        Some(&format!("multipart/form-data; boundary={boundary}")),
        multipart_body(boundary, &png),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("Continue to Bluesky"), "{page}");

    let par = seen
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.path == "/par")
        .map(|request| String::from_utf8_lossy(&request.body).to_string())
        .expect("PAR was sent");
    let state = form_value(&par, "state").expect("PAR carried state");
    let callback = format!(
        "/skin/callback?code=test-code&state={}&iss={}",
        url_encode(&state),
        url_encode(&stub_url)
    );
    let (status, page) = raw_request(&router, "GET", &callback, None, Vec::new()).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("Skin published"), "{page}");
    assert!(
        page.contains("In-game application is not available"),
        "{page}"
    );

    {
        let seen = seen.lock().unwrap();
        let upload = seen
            .iter()
            .find(|request| request.path == "/xrpc/com.atproto.repo.uploadBlob")
            .expect("the skin blob was uploaded");
        assert_eq!(upload.method, "POST");
        assert_eq!(upload.content_type.as_deref(), Some("image/png"));
        assert_eq!(upload.body, png);

        let put = seen
            .iter()
            .find(|request| request.path == "/xrpc/com.atproto.repo.putRecord")
            .expect("the skin record was written");
        assert_eq!(put.method, "POST");
        assert_eq!(put.content_type.as_deref(), Some("application/json"));
        let payload: Value = serde_json::from_slice(&put.body).unwrap();
        assert_eq!(payload["repo"], "did:plc:alice");
        assert_eq!(payload["collection"], "at.atpcraft.player.skin");
        assert_eq!(payload["rkey"], signer.key_id());
        assert_eq!(payload["validate"], false);
        assert_eq!(payload["record"]["model"], "slim");
        assert_eq!(payload["record"]["skin"]["ref"]["$link"], CID);
        assert!(payload["record"].get("cape").is_none());

        let record: SkinRecord = serde_json::from_value(payload["record"].clone()).unwrap();
        let keys = HashMap::from([(signer.key_id().to_string(), key.verifying_key())]);
        let endorsements = serde_json::to_value(&record.endorsements).unwrap();
        assert!(matches!(
            verify_endorsements(&endorsements, &keys, |server| {
                (server == signer.key_id())
                    .then(|| record.signable_payload("did:plc:alice", server))
            }),
            EndorsementVerdict::Verified { .. }
        ));
    }

    let (status, _) = raw_request(&router, "GET", &callback, None, Vec::new()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "OAuth state is single-use");
    let (status, _) = raw_request(&router, "GET", &skin_path, None, Vec::new()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a published upload link is consumed"
    );

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
