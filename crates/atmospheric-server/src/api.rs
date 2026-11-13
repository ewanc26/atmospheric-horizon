//! HTTP API for the server-side identity map.
//!
//! Public reads let a game client resolve a player UUID. Admin writes are
//! protected by `ATMOSPHERIC_SERVER_TOKEN`; a missing token disables only the
//! admin routes. Pairing claims are public because possession of the one-time
//! code is the credential.

use crate::{
    error::ApiError,
    model::{
        ClaimInput, Health, Link, LinkInput, LinksResponse, PairingInput, PairingResponse,
        normalize_uuid, unix_now,
    },
    store::LinkStore,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    store: LinkStore,
    admin_token: Option<Arc<str>>,
}

impl AppState {
    fn new(store: LinkStore, admin_token: Option<String>) -> Self {
        let admin_token = admin_token
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty())
            .map(Arc::from);
        Self { store, admin_token }
    }

    fn authorize(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let expected = self.admin_token.as_deref().ok_or(ApiError::AdminDisabled)?;
        let value = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let (scheme, actual) = value.split_once(' ').ok_or(ApiError::Unauthorized)?;
        let actual = actual.trim();
        if !scheme.eq_ignore_ascii_case("Bearer")
            || !constant_time_eq(expected.as_bytes(), actual.as_bytes())
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(())
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |mismatch, (left, right)| mismatch | (left ^ right))
        == 0
}

/// Build the complete router with the given store and admin token.
pub fn app(store: LinkStore, admin_token: Option<String>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/links", get(list_links))
        .route(
            "/v1/links/{uuid}",
            get(get_link).put(put_link).delete(delete_link),
        )
        .route("/v1/pairings", post(create_pairing))
        .route("/v1/pairings/claim", post(claim_pairing))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(AppState::new(store, admin_token))
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    let (links, pending_pairings) = state.store.counts(unix_now()).await;
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        links,
        pending_pairings,
    })
}

async fn get_link(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
) -> Result<Json<Link>, ApiError> {
    let uuid = normalize_uuid(&uuid)?;
    state
        .store
        .link(&uuid)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no link for {uuid}")))
}

async fn list_links(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LinksResponse>, ApiError> {
    state.authorize(&headers)?;
    let links = state.store.links().await;
    Ok(Json(LinksResponse {
        total: links.len(),
        links,
    }))
}

async fn put_link(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    Json(input): Json<LinkInput>,
) -> Result<(StatusCode, Json<Link>), ApiError> {
    state.authorize(&headers)?;
    let (link, existed) = state.store.upsert_link(&uuid, &input, unix_now()).await?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(link)))
}

async fn delete_link(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&headers)?;
    if state.store.remove_link(&uuid).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("no link for {uuid}")))
    }
}

async fn create_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PairingInput>,
) -> Result<(StatusCode, Json<PairingResponse>), ApiError> {
    state.authorize(&headers)?;
    let pairing = state.store.create_pairing(&input, unix_now()).await?;
    Ok((StatusCode::CREATED, Json(pairing)))
}

async fn claim_pairing(
    State(state): State<AppState>,
    Json(input): Json<ClaimInput>,
) -> Result<Json<Link>, ApiError> {
    Ok(Json(state.store.claim_pairing(&input, unix_now()).await?))
}

async fn not_found() -> ApiError {
    ApiError::NotFound("route not found".into())
}
