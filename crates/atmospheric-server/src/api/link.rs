use super::AppState;
use crate::{
    error::ApiError,
    model::{Link, LinkInput, LinksResponse, normalize_uuid, unix_now},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

pub(super) async fn get_link(
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

pub(super) async fn list_links(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LinksResponse>, ApiError> {
    state.authorize_admin(&headers)?;
    let links = state.store.links().await;
    Ok(Json(LinksResponse {
        total: links.len(),
        links,
    }))
}

pub(super) async fn put_link(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    Json(input): Json<LinkInput>,
) -> Result<(StatusCode, Json<Link>), ApiError> {
    state.authorize_admin(&headers)?;
    let (link, existed) = state.store.upsert_link(&uuid, &input, unix_now()).await?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(link)))
}

pub(super) async fn delete_link(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize_admin(&headers)?;
    if state.store.remove_link(&uuid).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("no link for {uuid}")))
    }
}
