//! HTTP routes for Continuity Watchman (#190).
//!
//! Reads are public because PLC data is public and the snapshot contains no
//! secrets. Watch/unwatch is a game-server control operation: the existing
//! `ATMOSPHERIC_SERVER_LINK_TOKEN` is accepted, with the static admin token as
//! a fallback for operators without browser linking.

use super::AppState;
use crate::{
    continuity::{ContinuityStatus, IdentityEvent, IdentitySnapshot, validate_watch_did},
    error::ApiError,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(super) struct WatchInput {
    did: String,
}

#[derive(Debug, Serialize)]
pub(super) struct WatchResponse {
    did: String,
    watched: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct EventsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct SnapshotsResponse {
    snapshots: Vec<IdentitySnapshot>,
}

#[derive(Debug, Serialize)]
pub(super) struct EventsResponse {
    events: Vec<IdentityEvent>,
}

/// Add a DID to the watch list. Only a DID is accepted; handles are resolved
/// from the identity map and never trusted as the identity key.
pub(super) async fn watch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<WatchInput>,
) -> Result<(StatusCode, Json<WatchResponse>), ApiError> {
    let continuity = state
        .continuity
        .as_ref()
        .ok_or(ApiError::ContinuityDisabled)?;
    state.authorize_watch(&headers)?;
    let did = validate_watch_did(&input.did).map_err(ApiError::Invalid)?;
    let uuid = state.store.uuid_for_did(&did).await;
    continuity
        .watch(&did, uuid)
        .await
        .map_err(ApiError::Invalid)?;
    Ok((
        StatusCode::CREATED,
        Json(WatchResponse { did, watched: true }),
    ))
}

/// Stop watching one DID. Existing snapshots and events remain readable.
pub(super) async fn unwatch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(did): Path<String>,
) -> Result<Json<WatchResponse>, ApiError> {
    let continuity = state
        .continuity
        .as_ref()
        .ok_or(ApiError::ContinuityDisabled)?;
    state.authorize_watch(&headers)?;
    let did = validate_watch_did(&did).map_err(ApiError::Invalid)?;
    continuity.unwatch(&did).await;
    Ok(Json(WatchResponse {
        did,
        watched: false,
    }))
}

/// All watched identities for which a snapshot has been observed.
pub(super) async fn snapshots(
    State(state): State<AppState>,
) -> Result<Json<SnapshotsResponse>, ApiError> {
    let continuity = state
        .continuity
        .as_ref()
        .ok_or(ApiError::ContinuityDisabled)?;
    Ok(Json(SnapshotsResponse {
        snapshots: continuity.snapshots().await,
    }))
}

/// One watched identity's current DID-keyed snapshot.
pub(super) async fn snapshot(
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<Json<IdentitySnapshot>, ApiError> {
    let continuity = state
        .continuity
        .as_ref()
        .ok_or(ApiError::ContinuityDisabled)?;
    let did = validate_watch_did(&did).map_err(ApiError::Invalid)?;
    let snapshot = continuity
        .snapshot(&did)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("no snapshot for {did}")))?;
    Ok(Json(snapshot))
}

/// Newest recorded identity changes, bounded by the configured event cap.
pub(super) async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    let continuity = state
        .continuity
        .as_ref()
        .ok_or(ApiError::ContinuityDisabled)?;
    Ok(Json(EventsResponse {
        events: continuity.events(query.limit.unwrap_or(50)).await,
    }))
}

/// Bounded in-memory watcher state and startup policy.
pub(super) async fn status(
    State(state): State<AppState>,
) -> Result<Json<ContinuityStatus>, ApiError> {
    let continuity = state
        .continuity
        .as_ref()
        .ok_or(ApiError::ContinuityDisabled)?;
    Ok(Json(continuity.status().await))
}
