//! End-to-end tests for Operator's Warrant (#189).
//!
//! The real router is driven without opening a port. Proof-of-Handle network
//! verification is injected as a stub; the #185 crate tests cover that chain.

use atmospheric::proof::{Challenge, HandleProof};
use atmospheric_server::{
    ServerConfig, app,
    operator::{GrantScope, Operator, OperatorSettings, Receipt, VerifyFn, VerifyFuture},
    store::LinkStore,
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "test-admin-token";
const DID: &str = "did:plc:operator123";
const UUID: &str = "00000000-0000-4000-8000-0000000000d1";

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "atmospheric-operator-e2e-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn verifier(expected_did: &'static str) -> VerifyFn {
    Arc::new(move |did: String, challenge: Challenge| -> VerifyFuture {
        assert_eq!(did, expected_did);
        assert!(!challenge.nonce.is_empty());
        Box::pin(async move {
            Ok(HandleProof {
                did,
                handle: "operator.example".into(),
                rev: "3JZRS4QKC0R77".into(),
                verified_at: challenge.issued_at,
            })
        })
    })
}

fn mismatched_verifier() -> VerifyFn {
    Arc::new(move |_did: String, challenge: Challenge| -> VerifyFuture {
        Box::pin(async move {
            Ok(HandleProof {
                did: "did:plc:somebody-else".into(),
                handle: "other.example".into(),
                rev: "3JZRS4QKC0R77".into(),
                verified_at: challenge.issued_at,
            })
        })
    })
}

fn config(operator: Option<Operator>) -> ServerConfig {
    ServerConfig {
        admin_token: Some(ADMIN_TOKEN.into()),
        link_token: None,
        public_url: None,
        oauth_resolver: None,
        labeler: None,
        skin_signer: None,
        operator,
    }
}

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = match body {
        Some(body) => serde_json::to_vec(&body).unwrap(),
        None => Vec::new(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, value)
}

async fn issue_challenge(router: &Router, did: &str) -> String {
    let (status, body) = request(
        router,
        "POST",
        "/v1/operator/challenge",
        None,
        Some(json!({ "did": did })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["nonce"].as_str().unwrap().to_owned()
}

async fn mint_grant(router: &Router, did: &str, nonce: &str) -> String {
    let (status, body) = request(
        router,
        "POST",
        "/v1/operator/grant",
        None,
        Some(json!({ "did": did, "nonce": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["did"], did);
    body["token"].as_str().unwrap().to_owned()
}

async fn verify_receipts(router: &Router, token: &str) -> Vec<Receipt> {
    let (status, key) = request(router, "GET", "/v1/operator/audit-key", None, None).await;
    assert_eq!(status, StatusCode::OK, "{key}");
    let key_id = key["key_id"].as_str().unwrap();
    let spki = key["public_key_spki_b64"].as_str().unwrap();

    let (status, body) = request(router, "GET", "/v1/operator/receipts", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let receipts: Vec<Receipt> =
        serde_json::from_value(body["receipts"].clone()).expect("receipts deserialize");
    for receipt in &receipts {
        receipt
            .verify(key_id, spki)
            .expect("valid receipt signature");
    }
    receipts
}

#[tokio::test]
async fn a_did_grant_authorizes_scoped_admin_routes_and_writes_receipts() {
    let dir = temp_dir("full-flow");
    let _ = std::fs::remove_dir_all(&dir);
    let settings = OperatorSettings {
        dids: vec![DID.into()],
        grant_ttl_secs: 60,
        scopes: GrantScope::all(),
    };
    let operator = Operator::open(dir.join("operator"), settings, verifier(DID)).unwrap();
    let store = LinkStore::open(dir.join("links.json")).unwrap();
    let router = app(store, config(Some(operator)));

    // Only allowlisted DIDs get a challenge.
    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/challenge",
        None,
        Some(json!({ "did": "did:plc:stranger" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let nonce = issue_challenge(&router, DID).await;
    // A wrong nonce consumes the challenge; the proof chain is never invoked.
    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/grant",
        None,
        Some(json!({ "did": DID, "nonce": "wrong" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/grant",
        None,
        Some(json!({ "did": DID, "nonce": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let nonce = issue_challenge(&router, DID).await;
    let grant = mint_grant(&router, DID, &nonce).await;
    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/grant",
        None,
        Some(json!({ "did": DID, "nonce": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, body) = request(
        &router,
        "PUT",
        &format!("/v1/links/{UUID}"),
        Some(&grant),
        Some(json!({
            "player_name": "Operator",
            "did": DID,
            "handle": "operator.example"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = request(&router, "GET", "/v1/links", Some(&grant), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);

    // The static token remains authoritative and does not write receipts.
    let (status, body) = request(&router, "GET", "/v1/links", Some(ADMIN_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let receipts = verify_receipts(&router, &grant).await;
    let mut actions: Vec<&str> = receipts
        .iter()
        .filter(|receipt| receipt.outcome == "ok")
        .map(|receipt| receipt.action.as_str())
        .collect();
    actions.sort_unstable();
    assert_eq!(actions, ["list_links", "put_link", "receipts"]);
    for receipt in &receipts {
        assert_eq!(receipt.did, DID);
        assert!(!receipt.grant_id.is_empty());
        assert!(!receipt.grant_id.contains(&grant));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_scope_denied_grant_writes_a_signed_denial_receipt() {
    let dir = temp_dir("scope-denied");
    let _ = std::fs::remove_dir_all(&dir);
    let settings = OperatorSettings {
        dids: vec![DID.into()],
        grant_ttl_secs: 60,
        scopes: vec![GrantScope::LinksWrite],
    };
    let operator = Operator::open(dir.join("operator"), settings, verifier(DID)).unwrap();
    let store = LinkStore::open(dir.join("links.json")).unwrap();
    let router = app(store, config(Some(operator)));

    let nonce = issue_challenge(&router, DID).await;
    let grant = mint_grant(&router, DID, &nonce).await;

    let (status, body) = request(&router, "GET", "/v1/links", Some(&grant), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("links_read"));

    let (status, body) = request(
        &router,
        "POST",
        "/v1/pairings",
        Some(&grant),
        Some(json!({ "player_uuid": UUID, "player_name": "Operator" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("pairing_create"));

    let (status, body) = request(
        &router,
        "POST",
        "/v1/labeler/labels",
        Some(&grant),
        Some(json!({ "uri": "at://did:plc:player/app.bsky.feed.post/abc", "val": "player" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("label_emit"));

    let (status, body) = request(
        &router,
        "PUT",
        &format!("/v1/links/{UUID}"),
        Some(&grant),
        Some(json!({
            "player_name": "Operator",
            "did": DID,
            "handle": "operator.example"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let receipts = verify_receipts(&router, ADMIN_TOKEN).await;
    let denial = receipts
        .iter()
        .find(|receipt| receipt.outcome == "scope_denied")
        .expect("scope denial receipt");
    assert_eq!(denial.action, "list_links");
    assert_eq!(denial.scope, "links_read");
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.outcome == "ok" && receipt.action == "put_link")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_proof_for_another_did_does_not_mint_a_grant() {
    let dir = temp_dir("wrong-did");
    let _ = std::fs::remove_dir_all(&dir);
    let operator = Operator::open(
        dir.join("operator"),
        OperatorSettings {
            dids: vec![DID.into()],
            grant_ttl_secs: 60,
            scopes: GrantScope::all(),
        },
        mismatched_verifier(),
    )
    .unwrap();
    let store = LinkStore::open(dir.join("links.json")).unwrap();
    let router = app(store, config(Some(operator)));

    let nonce = issue_challenge(&router, DID).await;
    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/grant",
        None,
        Some(json!({ "did": DID, "nonce": nonce })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, body) = request(&router, "GET", "/v1/links", Some(ADMIN_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn operator_routes_are_unavailable_without_an_allowlist() {
    let dir = temp_dir("disabled");
    let _ = std::fs::remove_dir_all(&dir);
    let store = LinkStore::open(dir.join("links.json")).unwrap();
    let router = app(store, config(None));

    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/challenge",
        None,
        Some(json!({ "did": DID })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    let (status, body) = request(
        &router,
        "POST",
        "/v1/operator/grant",
        None,
        Some(json!({ "did": DID, "nonce": "anything" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    let (status, body) = request(&router, "GET", "/v1/operator/audit-key", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    let (status, body) = request(
        &router,
        "GET",
        "/v1/operator/receipts",
        Some(ADMIN_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    let _ = std::fs::remove_dir_all(&dir);
}
