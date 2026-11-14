use super::AppState;
use crate::model::{Health, unix_now};
use axum::{Json, extract::State};

pub(super) async fn health(State(state): State<AppState>) -> Json<Health> {
    let (links, pending_pairings) = state.store.counts(unix_now()).await;
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        links,
        pending_pairings,
    })
}
