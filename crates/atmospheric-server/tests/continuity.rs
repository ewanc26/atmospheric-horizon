//! End-to-end tests for Continuity Watchman (#190).
//!
//! The real router is driven without opening a port. PLC network traffic is
//! absent: export operations are injected into the same in-memory state the
//! background watcher would fill.

use atmospheric_server::{
    ServerConfig, app,
    continuity::{Continuity, ContinuitySettings, ExportOperation},
    model::LinkInput,
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
const DID: &str = "did:plc:abcdefghijklmnopqrstuvwx";
const OTHER_DID: &str = "did:plc:zyxwvutsrqponmlkjihgfedc";
const UUID: &str = "00000000-0000-4000-8000-0000000000d1";

fn temp_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir()
        .join(format!(
            "atmospheric-continuity-e2e-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("links.json")
}

fn config(continuity: Option<Continuity>) -> ServerConfig {
    ServerConfig {
        admin_token: Some(ADMIN_TOKEN.into()),
        link_token: Some(LINK_TOKEN.into()),
        public_url: Some("https://links.example".into()),
        oauth_resolver: None,
        labeler: None,
        skin_signer: None,
        operator: None,
        continuity,
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

fn operation(did: &str, endpoint: &str, seq: u64) -> ExportOperation {
    ExportOperation {
        seq,
        did: did.into(),
        operation: json!({
            "type": "plc_operation",
            "rotationKeys": ["did:key:zRotation"],
            "alsoKnownAs": ["at://alice.test"],
            "verificationMethods": {"atproto": "did:key:zSigning"},
            "services": {
                "atproto_pds": {
                    "type": "AtprotoPersonalDataServer",
                    "endpoint": endpoint
                }
            }
        }),
        cid: format!("bafy{seq}"),
        created_at: "2026-11-25T00:00:00.000Z".into(),
    }
}

#[tokio::test]
async fn continuity_routes_are_disabled_until_the_feature_is_enabled() {
    let store = LinkStore::open(temp_path("disabled")).unwrap();
    let router = app(store, config(None));

    for path in [
        "/v1/continuity/status",
        "/v1/continuity/snapshots",
        "/v1/continuity/events",
    ] {
        let (status, body) = request(&router, "GET", path, None, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
    }
    let (status, body) = request(
        &router,
        "POST",
        "/v1/continuity/watch",
        Some(ADMIN_TOKEN),
        Some(json!({ "did": DID })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}

#[tokio::test]
async fn watching_requires_a_token_and_only_accepts_dids() {
    let store = LinkStore::open(temp_path("auth")).unwrap();
    let continuity = Continuity::with_settings(ContinuitySettings::default());
    let router = app(store, config(Some(continuity.clone())));

    let (status, body) = request(
        &router,
        "POST",
        "/v1/continuity/watch",
        None,
        Some(json!({ "did": DID })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, body) = request(
        &router,
        "POST",
        "/v1/continuity/watch",
        Some("wrong"),
        Some(json!({ "did": DID })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, body) = request(
        &router,
        "POST",
        "/v1/continuity/watch",
        Some(LINK_TOKEN),
        Some(json!({ "did": "alice.test" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = request(
        &router,
        "POST",
        "/v1/continuity/watch",
        Some(LINK_TOKEN),
        Some(json!({ "did": DID })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["did"], DID);
    assert_eq!(body["watched"], true);
    assert_eq!(continuity.watched().await.len(), 1);

    // The static admin token remains a valid fallback for operators who do not
    // run browser linking.
    let (status, body) = request(
        &router,
        "DELETE",
        &format!("/v1/continuity/watch/{DID}"),
        Some(ADMIN_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["watched"], false);
    assert!(continuity.watched().await.is_empty());
}

#[tokio::test]
async fn snapshots_and_events_are_did_keyed_and_drainable() {
    let store = LinkStore::open(temp_path("events")).unwrap();
    store
        .upsert_link(
            UUID,
            &LinkInput {
                player_name: "Alex".into(),
                did: DID.into(),
                handle: Some("alice.test".into()),
            },
            1,
        )
        .await
        .unwrap();
    let continuity = Continuity::new(ContinuitySettings::default(), 4);
    let router = app(store, config(Some(continuity.clone())));

    let (status, body) = request(
        &router,
        "POST",
        "/v1/continuity/watch",
        Some(LINK_TOKEN),
        Some(json!({ "did": DID })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    continuity
        .apply_export(&operation(DID, "https://old.example", 1), 10)
        .await;
    continuity
        .apply_export(&operation(DID, "https://new.example", 2), 20)
        .await;
    continuity
        .apply_export(&operation(OTHER_DID, "https://other.example", 3), 30)
        .await;

    let (status, body) = request(&router, "GET", "/v1/continuity/snapshots", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["snapshots"].as_array().unwrap().len(), 1);
    let snapshot = &body["snapshots"][0];
    assert_eq!(snapshot["did"], DID);
    assert_eq!(snapshot["uuid"], UUID);
    assert_eq!(snapshot["pds"], "https://new.example");
    assert_eq!(snapshot["seq"], 2);

    let (status, body) = request(
        &router,
        "GET",
        &format!("/v1/continuity/snapshots/{DID}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["did"], DID);
    let (status, body) = request(
        &router,
        "GET",
        &format!("/v1/continuity/snapshots/{OTHER_DID}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = request(&router, "GET", "/v1/continuity/events?limit=1", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["did"], DID);
    assert_eq!(events[0]["changes"][0], "pds_migration");
    assert_eq!(events[0]["previous"]["pds"], "https://old.example");
    assert_eq!(events[0]["current"]["pds"], "https://new.example");

    let (status, body) = request(&router, "GET", "/v1/continuity/status", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["watched"], 1);
    assert_eq!(body["snapshots"], 1);
    assert_eq!(body["events"], 2);
    assert_eq!(body["cursor"], 3);
    assert_eq!(body["pollSecs"], 300);
    assert_eq!(body["jitterSecs"], 30);

    // Unwatch leaves the last snapshot and its events readable so a game
    // server can drain them after a player leaves.
    let (status, body) = request(
        &router,
        "DELETE",
        &format!("/v1/continuity/watch/{DID}"),
        Some(LINK_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = request(&router, "GET", "/v1/continuity/snapshots", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["snapshots"].as_array().unwrap().is_empty());
    let (status, body) = request(
        &router,
        "GET",
        &format!("/v1/continuity/snapshots/{DID}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = request(&router, "GET", "/v1/continuity/events", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["events"].as_array().unwrap().len(), 2);
}
