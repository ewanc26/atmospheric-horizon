use super::AppState;
use crate::{
    error::ApiError,
    labeler::{EmitLabel, Labeler, LabelerError, LogEntry, Query, error_frame, labels_frame},
    model::{EmitLabelInput, unix_now},
};
use axum::{
    Json,
    extract::{
        RawQuery, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio::sync::broadcast;

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

/// Public spec WebSocket. A `cursor` backfills the durable log first; labels
/// emitted while that runs are buffered and only sent after the backfill, with
/// duplicates skipped by sequence number.
pub(super) async fn subscribe_labels(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    RawQuery(raw): RawQuery,
) -> Response {
    let Some(labeler) = state.labeler.clone() else {
        return xrpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NotEnabled",
            "Labeler service not initialized",
        );
    };
    let cursor = Query::subscribe_cursor(raw.as_deref().unwrap_or_default());
    ws.on_upgrade(move |socket| stream_labels(socket, labeler, cursor))
}

async fn stream_labels(mut socket: WebSocket, labeler: Labeler, cursor: Option<u64>) {
    let mut subscription = labeler.subscribe();
    let last_seq = labeler.last_seq().await;

    if let Some(cursor) = cursor
        && cursor > last_seq
    {
        let message = format!("Cursor {cursor} is past the current sequence ({last_seq})");
        let _ = socket
            .send(Message::Binary(
                error_frame("FutureCursor", Some(&message)).into(),
            ))
            .await;
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 1000,
                reason: "FutureCursor".into(),
            })))
            .await;
        return;
    }

    let mut last_sent = cursor.unwrap_or(0);
    for entry in labeler.entries_after(last_sent).await {
        if send_labels(&mut socket, &entry).await.is_err() {
            return;
        }
        last_sent = entry.seq;
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
            live = subscription.recv() => match live {
                Ok(entry) if entry.seq > last_sent => {
                    if send_labels(&mut socket, &entry).await.is_err() {
                        return;
                    }
                    last_sent = entry.seq;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = socket
                        .send(Message::Close(Some(CloseFrame {
                            code: 1013,
                            reason: "lagged".into(),
                        })))
                        .await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

async fn send_labels(socket: &mut WebSocket, entry: &LogEntry) -> Result<(), axum::Error> {
    socket
        .send(Message::Binary(labels_frame(entry).into()))
        .await
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
