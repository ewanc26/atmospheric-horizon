use super::AppState;
use crate::{
    error::ApiError,
    labeler::{EmitLabel, LabelerError, LogEntry, Query},
    model::{EmitLabelInput, unix_now},
};
use axum::{
    Json,
    extract::{RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Public spec endpoint. Errors use the XRPC `{error, message}` envelope.
pub(super) async fn query_labels(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Response {
    let Some(labeler) = state.labeler.as_ref() else {
        return xrpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NotEnabled",
            "Labeler service not initialized",
        );
    };
    let query = match Query::parse(raw.as_deref().unwrap_or_default()) {
        Ok(query) => query,
        Err(message) => {
            return xrpc_error(StatusCode::BAD_REQUEST, "InvalidRequest", message);
        }
    };
    let result = labeler.query(&query, unix_now()).await;
    (StatusCode::OK, Json(result)).into_response()
}

/// Admin write. The configured labeler DID is the `src`; the server stamps
/// `cts` and signs the canonical unsigned label before appending it.
pub(super) async fn emit_label(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<EmitLabelInput>,
) -> Result<(StatusCode, Json<LogEntry>), ApiError> {
    state.authorize_admin(&headers)?;
    let labeler = state.labeler.as_ref().ok_or(ApiError::LabelerDisabled)?;
    let entry = labeler
        .emit(
            EmitLabel {
                uri: input.uri,
                val: input.val,
                neg: input.neg,
                cid: input.cid,
                exp: input.exp,
            },
            unix_now(),
        )
        .await
        .map_err(|error| match error {
            LabelerError::Invalid(message) => ApiError::Invalid(message),
            other => {
                eprintln!("atmospheric-server: label emission failed: {other}");
                ApiError::Internal
            }
        })?;
    Ok((StatusCode::CREATED, Json(entry)))
}

fn xrpc_error(status: StatusCode, error: &'static str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": error,
            "message": message.into(),
        })),
    )
        .into_response()
}
