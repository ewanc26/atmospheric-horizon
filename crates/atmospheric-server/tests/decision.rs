//! End-to-end tests for the Ledger of Decisions sidecar foundation (#199).
//!
//! The real router is driven without opening a port. No PDS or network traffic
//! is involved: records are signed and persisted under a temporary data dir.

use atmospheric_server::{
    ServerConfig, app,
    decision::{DecisionLedger, DecisionSettings},
    store::LinkStore,
};
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "test-admin-token";
const LINK_TOKEN: &str = "test-link-token";
const ISSUER: &str = "did:plc:server";

fn temp_paths(tag: &str) -> (PathBuf, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "atmospheric-decision-e2e-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    (dir.clone(), dir.join("links.json"))
}

async fn request(
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
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "non-JSON response ({status:?}): {:?} ({error})",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, value)
}

fn config(ledger: Option<DecisionLedger>) -> ServerConfig {
    ServerConfig {
        admin_token: Some(ADMIN_TOKEN.into()),
        link_token: Some(LINK_TOKEN.into()),
        public_url: Some("https://links.example".into()),
        oauth_resolver: None,
        labeler: None,
        skin_signer: None,
        operator: None,
        continuity: None,
        ledger,
    }
}

#[tokio::test]
async fn ledger_routes_are_disabled_by_default() {
    let (dir, links) = temp_paths("disabled");
    let router = app(LinkStore::open(&links).unwrap(), config(None));
    let (status, body) = request(&router, "GET", "/v1/decisions/status", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "internal server error");
    let (status, _) = request(
        &router,
        "POST",
        "/v1/decisions",
        Some(ADMIN_TOKEN),
        Some(json!({
            "kind": "decision",
            "subject": "did:plc:alice",
            "category": "spam",
            "scope": "server"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn decisions_are_signed_private_and_resolved() {
    let (dir, links) = temp_paths("submit");
    let settings = DecisionSettings::new(ISSUER).unwrap();
    let ledger = DecisionLedger::open(&dir, &settings).unwrap();
    let router = app(LinkStore::open(&links).unwrap(), config(Some(ledger)));

    let (status, key) = request(&router, "GET", "/v1/decisions/key", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["key_id"].as_str().unwrap().len(), 16);
    assert!(key["public_key_spki_b64"].as_str().unwrap().len() > 32);

    let (status, created) = request(
        &router,
        "POST",
        "/v1/decisions",
        Some(LINK_TOKEN),
        Some(json!({
            "kind": "decision",
            "subject": "did:plc:alice",
            "category": "spam",
            "scope": "server"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let rkey = created["rkey"].as_str().unwrap().to_owned();
    let subject = created["record"]["subject"].as_str().unwrap().to_owned();
    assert_eq!(subject.len(), 64);
    assert_eq!(created["record"]["issuer"], ISSUER);
    assert_eq!(
        created["record"]["collection"],
        "club.atmospheric.mod.decision"
    );
    assert!(
        !serde_json::to_string(&created)
            .unwrap()
            .contains("did:plc:alice")
    );

    // A plaintext DID as the subject is refused; only the DID-to-hash step
    // inside the ledger may see it.
    let (status, _) = request(
        &router,
        "POST",
        "/v1/decisions",
        Some(ADMIN_TOKEN),
        Some(json!({
            "kind": "decision",
            "subject": "@alice",
            "category": "spam",
            "scope": "server"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Unauthenticated writes are refused, but public reads still work.
    let (status, _) = request(
        &router,
        "POST",
        "/v1/decisions",
        None,
        Some(json!({
            "kind": "decision",
            "subject": "did:plc:bob",
            "category": "spam",
            "scope": "server"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, list) = request(&router, "GET", "/v1/decisions", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(list["decisions"][0]["state"], "active");
    assert_eq!(list["decisions"][0]["rkey"], rkey);

    let (status, filtered) = request(
        &router,
        "GET",
        &format!("/v1/decisions?subject={subject}&state=active"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered["decisions"].as_array().unwrap().len(), 1);
    let (status, filtered) = request(
        &router,
        "GET",
        "/v1/decisions?state=active&subject=not-a-subject",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(filtered["decisions"].as_array().unwrap().is_empty());
    let (status, _) = request(&router, "GET", "/v1/decisions?state=bogus", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, raw) = request(&router, "GET", &format!("/v1/decisions/{rkey}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw["rkey"], rkey);

    let (status, reversed) = request(
        &router,
        "POST",
        "/v1/decisions",
        Some(ADMIN_TOKEN),
        Some(json!({
            "kind": "reversal",
            "target": rkey,
            "category": "appeal upheld",
            "scope": "server"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{reversed}");
    assert_eq!(reversed["record"]["subject"], subject);

    let (status, list) = request(&router, "GET", "/v1/decisions", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["decisions"][0]["state"], "reversed");
    assert_eq!(
        list["decisions"][0]["endedBy"],
        reversed["rkey"].as_str().unwrap()
    );

    let (status, status_body) = request(&router, "GET", "/v1/decisions/status", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(status_body["issuer"], ISSUER);
    assert_eq!(status_body["records"], 2);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn ending_an_unknown_decision_is_not_found() {
    let (dir, links) = temp_paths("missing");
    let settings = DecisionSettings::new(ISSUER).unwrap();
    let ledger = DecisionLedger::open(&dir, &settings).unwrap();
    let router = app(LinkStore::open(&links).unwrap(), config(Some(ledger)));
    let (status, body) = request(
        &router,
        "POST",
        "/v1/decisions",
        Some(ADMIN_TOKEN),
        Some(json!({
            "kind": "expiry",
            "target": "2unknown00000",
            "category": "time served",
            "scope": "server"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("no decision"));
    let _ = std::fs::remove_dir_all(dir);
}
