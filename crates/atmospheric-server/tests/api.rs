//! End-to-end HTTP tests against the real router, without opening a TCP port.

use atmospheric_server::{ServerConfig, app, store::LinkStore};
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

const UUID_1: &str = "00000000-0000-4000-8000-000000000001";
const UUID_2: &str = "00000000-0000-4000-8000-000000000002";
const UUID_3: &str = "00000000-0000-4000-8000-000000000003";
const TOKEN: &str = "test-admin-token";
const LINK_TOKEN: &str = "test-link-token";

fn config() -> ServerConfig {
    ServerConfig {
        admin_token: Some(TOKEN.into()),
        link_token: Some(LINK_TOKEN.into()),
        public_url: Some("https://links.example".into()),
        oauth_resolver: None,
    }
}

fn temp_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir()
        .join(format!(
            "atmospheric-server-api-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("links.json")
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
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn links_are_public_but_writes_need_the_admin_token() {
    let path = temp_path("auth");
    let store = LinkStore::open(&path).unwrap();
    let router = app(store, config());

    let (status, body) = request(&router, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["links"], 0);

    let (status, _) = request(&router, "GET", "/missing", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let link = json!({
        "player_name": "Alex",
        "did": "did:plc:alice",
        "handle": "alice.test"
    });
    let (status, _) = request(
        &router,
        "PUT",
        &format!("/v1/links/{UUID_1}"),
        None,
        Some(link.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = request(
        &router,
        "PUT",
        &format!("/v1/links/{UUID_1}"),
        Some("wrong"),
        Some(link.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = request(
        &router,
        "PUT",
        &format!("/v1/links/{UUID_1}"),
        Some(TOKEN),
        Some(link),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["did"], "did:plc:alice");

    let (status, body) = request(&router, "GET", &format!("/v1/links/{UUID_1}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["player_uuid"], UUID_1);
    assert_eq!(body["handle"], "alice.test");

    let (status, _) = request(&router, "GET", "/v1/links", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = request(&router, "GET", "/v1/links", Some(TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);

    let (status, _) = request(
        &router,
        "PUT",
        &format!("/v1/links/{UUID_1}"),
        Some(TOKEN),
        Some(json!({
            "player_name": "Alex",
            "did": "did:plc:alice2",
            "handle": null
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(&router, "GET", "/v1/links/not-a-uuid", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &router,
        "DELETE",
        &format!("/v1/links/{UUID_1}"),
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = request(&router, "GET", &format!("/v1/links/{UUID_1}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

async fn raw_request(
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
async fn browser_links_need_the_shared_game_server_token() {
    let path = temp_path("browser");
    let store = LinkStore::open(&path).unwrap();
    let router = app(store, config());

    let (status, _) = request(
        &router,
        "POST",
        "/v1/browser/pairings",
        None,
        Some(json!({ "player_uuid": UUID_1, "player_name": "Alex" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = request(
        &router,
        "POST",
        "/v1/browser/pairings",
        Some(TOKEN),
        Some(json!({ "player_uuid": UUID_1, "player_name": "Alex" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = request(
        &router,
        "POST",
        "/v1/browser/pairings",
        Some(LINK_TOKEN),
        Some(json!({ "player_uuid": UUID_1, "player_name": "Alex" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let url = body["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("https://links.example/link/"));
    assert!(body["expires_at"].as_u64().unwrap() > 0);

    let link_path = url.trim_start_matches("https://links.example");
    let (status, page) = raw_request(&router, "GET", link_path, None, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Alex"));
    assert!(page.contains("name=\"handle\""));

    let (status, metadata) =
        request(&router, "GET", "/oauth-client-metadata.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        metadata["client_id"],
        "https://links.example/oauth-client-metadata.json"
    );
    assert_eq!(
        metadata["redirect_uris"][0],
        "https://links.example/link/callback"
    );

    let (status, _) = raw_request(
        &router,
        "GET",
        "/link/callback?state=unknown&code=code",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = raw_request(&router, "GET", "/link/not-a-token", None, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn pairing_claims_once_and_survives_a_restart() {
    let path = temp_path("pairing");
    let store = LinkStore::open(&path).unwrap();
    let router = app(store.clone(), config());

    let (status, body) = request(
        &router,
        "POST",
        "/v1/pairings",
        Some(TOKEN),
        Some(json!({
            "player_uuid": UUID_2,
            "player_name": "Steve",
            "ttl_seconds": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let code = body["code"].as_str().unwrap().to_owned();

    let (status, body) = request(
        &router,
        "POST",
        "/v1/pairings/claim",
        None,
        Some(json!({
            "code": code.to_lowercase(),
            "did": "did:plc:bob",
            "handle": "Bob.Test"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["player_name"], "Steve");
    assert_eq!(body["handle"], "bob.test");

    let (status, _) = request(
        &router,
        "POST",
        "/v1/pairings/claim",
        None,
        Some(json!({
            "code": code,
            "did": "did:plc:other",
            "handle": null
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(
        &router,
        "POST",
        "/v1/pairings",
        Some(TOKEN),
        Some(json!({
            "player_uuid": UUID_3,
            "player_name": "Herobrine"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = request(
        &router,
        "POST",
        "/v1/pairings",
        Some(TOKEN),
        Some(json!({
            "player_uuid": UUID_3,
            "player_name": "Herobrine"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let code = body["code"].as_str().unwrap().to_owned();
    let (status, _) = request(
        &router,
        "POST",
        "/v1/pairings/claim",
        None,
        Some(json!({
            "code": code,
            "did": "not-a-did",
            "handle": null
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &router,
        "POST",
        "/v1/pairings/claim",
        None,
        Some(json!({
            "code": code,
            "did": "did:plc:herobrine",
            "handle": null
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop(router);
    drop(store);
    let reopened = LinkStore::open(&path).unwrap();
    let link = reopened.link(UUID_2).await.unwrap().unwrap();
    assert_eq!(link.did, "did:plc:bob");
    assert!(reopened.link(UUID_3).await.unwrap().is_some());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
