//! HTTP routes for Operator's Warrant (#189).
//!
//! These routes are additive: the challenge and grant exchange is public (the
//! DID allowlist and PDS proof are the credential), while reading receipts is
//! an admin operation scoped to `audit_read`.

use super::AppState;
use crate::{
    error::ApiError,
    operator::{GrantScope, Receipt},
};
use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(super) struct ChallengeInput {
    did: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ChallengeResponse {
    did: String,
    nonce: String,
    issued_at: u64,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct GrantInput {
    did: String,
    nonce: String,
}

#[derive(Debug, Serialize)]
pub(super) struct GrantResponse {
    token: String,
    did: String,
    scopes: Vec<GrantScope>,
    issued_at: u64,
    expires_at: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct ReceiptsResponse {
    receipts: Vec<Receipt>,
}

#[derive(Debug, Serialize)]
pub(super) struct AuditKeyResponse {
    key_id: String,
    public_key_spki_b64: String,
}

/// Start a DID login by issuing a one-time Proof-of-Handle challenge.
pub(super) async fn challenge(
    State(state): State<AppState>,
    Json(input): Json<ChallengeInput>,
) -> Result<Json<ChallengeResponse>, ApiError> {
    let operator = state.operator.as_ref().ok_or(ApiError::OperatorDisabled)?;
    let did = input.did.trim().to_owned();
    let issued = operator.challenge(&did)?;
    Ok(Json(ChallengeResponse {
        did,
        nonce: issued.nonce,
        issued_at: issued.issued_at,
        expires_at: issued
            .issued_at
            .saturating_add(crate::operator::CHALLENGE_TTL_SECS),
    }))
}

/// Exchange a verified PDS proof for a short-lived scoped admin bearer token.
pub(super) async fn grant(
    State(state): State<AppState>,
    Json(input): Json<GrantInput>,
) -> Result<(axum::http::StatusCode, Json<GrantResponse>), ApiError> {
    let operator = state.operator.as_ref().ok_or(ApiError::OperatorDisabled)?;
    let did = input.did.trim().to_owned();
    let (token, grant) = operator.grant(&did, input.nonce.trim()).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(GrantResponse {
            token,
            did: grant.did,
            scopes: grant.scopes,
            issued_at: grant.issued_at,
            expires_at: grant.expires_at,
        }),
    ))
}

/// Read the signed receipts written when grants exercise admin operations.
pub(super) async fn receipts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ReceiptsResponse>, ApiError> {
    state
        .authorize_admin(&headers, GrantScope::AuditRead, "receipts", "list")
        .await?;
    let operator = state.operator.as_ref().ok_or(ApiError::OperatorDisabled)?;
    Ok(Json(ReceiptsResponse {
        receipts: operator.audit.receipts().await?,
    }))
}

/// Public key for verifying the signatures on audit receipts.
pub(super) async fn audit_key(
    State(state): State<AppState>,
) -> Result<Json<AuditKeyResponse>, ApiError> {
    let operator = state.operator.as_ref().ok_or(ApiError::OperatorDisabled)?;
    Ok(Json(AuditKeyResponse {
        key_id: operator.audit.key_id().to_owned(),
        public_key_spki_b64: operator.audit.public_key_spki_b64(),
    }))
}
