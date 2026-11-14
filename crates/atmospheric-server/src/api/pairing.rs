use super::AppState;
use crate::{
    error::ApiError,
    model::{ClaimInput, Link, PairingInput, PairingResponse, unix_now},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};

pub(super) async fn create_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PairingInput>,
) -> Result<(StatusCode, Json<PairingResponse>), ApiError> {
    state.authorize_admin(&headers)?;
    let pairing = state.store.create_pairing(&input, unix_now()).await?;
    Ok((StatusCode::CREATED, Json(pairing)))
}

pub(super) async fn claim_pairing(
    State(state): State<AppState>,
    Json(input): Json<ClaimInput>,
) -> Result<Json<Link>, ApiError> {
    Ok(Json(state.store.claim_pairing(&input, unix_now()).await?))
}
