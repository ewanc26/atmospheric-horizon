//! HTTP routes for Ledger of Decisions (#199).
//!
//! Reads are public: the records carry no plaintext handle or DID, and the
//! signing key is published so any reader can verify them. Submitting a record
//! is an admin operation, accepting either the static token, the game server's
//! link token, or a DID grant carrying the `ledger_write` scope.

use super::AppState;
use crate::{decision::DecisionInput, error::ApiError};
use atmospheric::decision::record::{DecisionEntry, DecisionState, ResolvedDecision};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Deserialize)]
pub(super) struct DecisionsQuery {
    subject: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct DecisionsResponse {
    decisions: Vec<ResolvedDecision>,
}

#[derive(Debug, Serialize)]
pub(super) struct LedgerKeyResponse {
    key_id: String,
    public_key_spki_b64: String,
}

/// Sign and append a moderation record. The plaintext subject DID is consumed
/// here and never persisted or returned: only its salted identifier is stored.
pub(super) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<DecisionInput>,
) -> Result<(StatusCode, Json<DecisionEntry>), ApiError> {
    let ledger = state.ledger.as_ref().ok_or(ApiError::LedgerDisabled)?;
    state.authorize_ledger(&headers).await?;
    let entry = ledger.submit(input).await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// Public current-state query, newest first.
pub(super) async fn decisions(
    State(state): State<AppState>,
    Query(query): Query<DecisionsQuery>,
) -> Result<Json<DecisionsResponse>, ApiError> {
    let ledger = state.ledger.as_ref().ok_or(ApiError::LedgerDisabled)?;
    let state_filter = query
        .state
        .as_deref()
        .map(DecisionState::from_str)
        .transpose()
        .map_err(ApiError::Invalid)?;
    Ok(Json(DecisionsResponse {
        decisions: ledger
            .resolved(
                query.subject.as_deref(),
                state_filter,
                query.limit.unwrap_or(50),
            )
            .await,
    }))
}

/// One raw signed record by repository key.
pub(super) async fn decision(
    State(state): State<AppState>,
    Path(rkey): Path<String>,
) -> Result<Json<DecisionEntry>, ApiError> {
    let ledger = state.ledger.as_ref().ok_or(ApiError::LedgerDisabled)?;
    ledger
        .entry(&rkey)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("no decision with rkey {rkey}")))
        .map(Json)
}

/// Public verification key for every signed ledger record.
pub(super) async fn ledger_key(
    State(state): State<AppState>,
) -> Result<Json<LedgerKeyResponse>, ApiError> {
    let ledger = state.ledger.as_ref().ok_or(ApiError::LedgerDisabled)?;
    Ok(Json(LedgerKeyResponse {
        key_id: ledger.key_id().to_owned(),
        public_key_spki_b64: ledger.public_key_spki_b64(),
    }))
}

/// Enabled ledger status: issuer, key id and record count.
pub(super) async fn status(
    State(state): State<AppState>,
) -> Result<Json<crate::decision::LedgerStatus>, ApiError> {
    let ledger = state.ledger.as_ref().ok_or(ApiError::LedgerDisabled)?;
    Ok(Json(ledger.status().await))
}
