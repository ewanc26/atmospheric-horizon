use super::AppState;
use crate::{
    error::ApiError,
    model::{
        BrowserPairingInput, BrowserPairingResponse, LinkInput, normalize_player_name,
        normalize_uuid, unix_now,
    },
    oauth::{self, Identity},
    page::{browser_page, html_escape},
};
use axum::{
    Form, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

pub(super) async fn create_browser_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<BrowserPairingInput>,
) -> Result<(StatusCode, Json<BrowserPairingResponse>), ApiError> {
    state.authorize_link(&headers)?;
    let player_uuid = normalize_uuid(&input.player_uuid)?;
    let player_name = normalize_player_name(&input.player_name)?;
    let (token, expires_at) = state.browser.create(&player_uuid, &player_name);
    let public_url = state
        .public_url
        .as_deref()
        .ok_or(ApiError::BrowserDisabled)?;
    let response = BrowserPairingResponse {
        url: format!("{public_url}/link/{token}"),
        expires_at,
    };
    Ok((StatusCode::CREATED, Json(response)))
}

pub(super) async fn client_metadata(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let public_url = state
        .public_url
        .as_deref()
        .ok_or(ApiError::BrowserDisabled)?;
    Ok(Json(oauth::client_metadata(public_url)))
}

pub(super) async fn link_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    let Some(pending) = state.browser.pending(&token) else {
        return browser_page(
            StatusCode::NOT_FOUND,
            "Link expired",
            "This link has expired or was already used. Ask the server for a new one in game.",
        );
    };
    let player = html_escape(&pending.player_name);
    let action = html_escape(&format!("/link/{token}"));
    browser_page(
        StatusCode::OK,
        "Link your Bluesky account",
        &format!(
            "<p>Signing in will link your Bluesky account to Minecraft player <strong>{player}</strong>.</p>\
             <form method=\"post\" action=\"{action}\">\
             <label for=\"handle\">Bluesky handle</label>\
             <input id=\"handle\" name=\"handle\" type=\"text\" autocomplete=\"username\" \
              placeholder=\"alice.bsky.social\" required>\
             <button type=\"submit\">Continue with Bluesky</button>\
             </form>"
        ),
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct LinkForm {
    handle: String,
}

pub(super) async fn start_browser_link(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Form(form): Form<LinkForm>,
) -> Response {
    let Some(_pending) = state.browser.pending(&token) else {
        return browser_page(
            StatusCode::NOT_FOUND,
            "Link expired",
            "This link has expired or was already used. Ask the server for a new one in game.",
        );
    };
    let Some(client) = &state.oauth else {
        return browser_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Browser linking is disabled",
            "The server has no public URL configured.",
        );
    };
    let started = match client.start(&form.handle).await {
        Ok(started) => started,
        Err(error) => {
            eprintln!("atmospheric-server: browser OAuth start failed: {error}");
            return browser_page(
                StatusCode::BAD_GATEWAY,
                "Could not start Bluesky login",
                "Check the handle and try again. The server could not reach the Bluesky authorization service.",
            );
        }
    };
    let target = html_escape(&started.authorize_url);
    state.browser.begin_auth(&token, started.pending);
    browser_page(
        StatusCode::OK,
        "Continue to Bluesky",
        &format!(
            "<p>Taking you to Bluesky to sign in…</p>\
             <p><a href=\"{target}\">Continue</a></p>\
             <meta http-equiv=\"refresh\" content=\"0;url={target}\">"
        ),
    )
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub(super) async fn link_callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = query.error.as_deref() {
        let description = query
            .error_description
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(error);
        return browser_page(
            StatusCode::BAD_REQUEST,
            "Bluesky sign-in failed",
            &html_escape(description),
        );
    }
    let (Some(code), Some(auth_state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return browser_page(
            StatusCode::BAD_REQUEST,
            "Incomplete sign-in",
            "Bluesky did not return the expected authorization response. Start again from the link in game.",
        );
    };
    let Some((link_token, pending)) = state.browser.take_auth(auth_state) else {
        return browser_page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "This sign-in attempt expired or was already used. Start again from the link in game.",
        );
    };
    let Some(client) = &state.oauth else {
        return browser_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Browser linking is disabled",
            "The server has no public URL configured.",
        );
    };
    let identity: Identity = match client.finish(&pending, code, query.iss.as_deref()).await {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("atmospheric-server: browser OAuth callback failed: {error}");
            return browser_page(
                StatusCode::BAD_GATEWAY,
                "Could not finish sign-in",
                "The authorization service refused this sign-in. Start again from the link in game.",
            );
        }
    };
    let Some(link) = state.browser.take_link(&link_token) else {
        return browser_page(
            StatusCode::GONE,
            "Link expired",
            "This link expired before sign-in finished. Ask the server for a new one in game.",
        );
    };
    let input = LinkInput {
        player_name: link.player_name,
        did: identity.did,
        handle: Some(identity.handle),
    };
    match state
        .store
        .upsert_link(&link.player_uuid, &input, unix_now())
        .await
    {
        Ok((link, _)) => browser_page(
            StatusCode::OK,
            "Bluesky account linked",
            &format!(
                "Minecraft player <strong>{}</strong> is linked to <strong>{}</strong>. You can close this page and return to the game.",
                html_escape(&link.player_name),
                html_escape(&link.did)
            ),
        ),
        Err(error) => {
            eprintln!("atmospheric-server: could not save browser link: {error}");
            browser_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save the link",
                "The identity service could not save this link. Ask the server operator to check its data file.",
            )
        }
    }
}
