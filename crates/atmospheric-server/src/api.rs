//! HTTP API for the server-side identity map and browser linking.
//!
//! Public reads let a game client resolve a player UUID. Admin writes are
//! protected by `ATMOSPHERIC_SERVER_TOKEN`; a missing token disables only the
//! admin routes. Pairing claims are public because possession of the one-time
//! code is the credential. Browser links are created only for the game server,
//! authenticated with `ATMOSPHERIC_SERVER_LINK_TOKEN`, and are then completed
//! by the player's browser OAuth login.

use crate::{
    browser::BrowserLinks,
    error::ApiError,
    model::{
        BrowserPairingInput, BrowserPairingResponse, ClaimInput, Health, Link, LinkInput,
        LinksResponse, PairingInput, PairingResponse, normalize_player_name, normalize_uuid,
        unix_now,
    },
    oauth::{self, Identity, OAuthClient, Resolver},
    store::LinkStore,
};
use axum::{
    Form, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::Arc;

const LINK_PAGE_LIMIT: usize = 16 * 1024;

/// Service configuration. All fields are optional so the identity map can run
/// with browser linking disabled, exactly as before M39.
#[derive(Clone, Debug, Default)]
pub struct ServerConfig {
    /// Bearer token for admin writes and operator pairing creation.
    pub admin_token: Option<String>,
    /// Bearer token the game server uses to create browser links.
    pub link_token: Option<String>,
    /// Public base URL of this service, used for OAuth client metadata and
    /// redirects (for example `https://links.example`).
    pub public_url: Option<String>,
    /// Optional resolver override. Production uses the public Bluesky
    /// services; integration tests point handle and DID resolution at a stub.
    #[doc(hidden)]
    pub oauth_resolver: Option<Resolver>,
}

#[derive(Clone)]
pub struct AppState {
    store: LinkStore,
    browser: BrowserLinks,
    admin_token: Option<Arc<str>>,
    link_token: Option<Arc<str>>,
    public_url: Option<Arc<str>>,
    oauth: Option<OAuthClient>,
}

impl AppState {
    fn new(store: LinkStore, config: ServerConfig) -> Self {
        let token = |value: Option<String>| {
            value
                .map(|token| token.trim().to_owned())
                .filter(|token| !token.is_empty())
                .map(Arc::from)
        };
        let public_url = config
            .public_url
            .map(|url| url.trim().trim_end_matches('/').to_owned())
            .filter(|url| !url.is_empty())
            .map(Arc::from);
        let resolver = config.oauth_resolver.unwrap_or_default();
        let oauth = public_url
            .as_deref()
            .map(|url| OAuthClient::new(url, resolver));
        Self {
            store,
            browser: BrowserLinks::default(),
            admin_token: token(config.admin_token),
            link_token: token(config.link_token),
            public_url,
            oauth,
        }
    }

    fn authorize(&self, headers: &HeaderMap, expected: Option<&str>) -> Result<(), ApiError> {
        let expected = expected.ok_or(ApiError::AdminDisabled)?;
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

    fn authorize_admin(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        self.authorize(headers, self.admin_token.as_deref())
    }

    fn authorize_link(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        if self.link_token.is_none() || self.public_url.is_none() {
            return Err(ApiError::BrowserDisabled);
        }
        self.authorize(headers, self.link_token.as_deref())
            .map_err(|error| match error {
                ApiError::AdminDisabled => ApiError::BrowserDisabled,
                other => other,
            })
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

/// Build the complete router with the given store and service configuration.
pub fn app(store: LinkStore, config: ServerConfig) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/links", get(list_links))
        .route(
            "/v1/links/{uuid}",
            get(get_link).put(put_link).delete(delete_link),
        )
        .route("/v1/pairings", post(create_pairing))
        .route("/v1/pairings/claim", post(claim_pairing))
        .route("/v1/browser/pairings", post(create_browser_pairing))
        .route("/oauth-client-metadata.json", get(client_metadata))
        .route("/link/callback", get(link_callback))
        .route("/link/{token}", get(link_page).post(start_browser_link))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(LINK_PAGE_LIMIT))
        .with_state(AppState::new(store, config))
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
    state.authorize_admin(&headers)?;
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
    state.authorize_admin(&headers)?;
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
    state.authorize_admin(&headers)?;
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
    state.authorize_admin(&headers)?;
    let pairing = state.store.create_pairing(&input, unix_now()).await?;
    Ok((StatusCode::CREATED, Json(pairing)))
}

async fn claim_pairing(
    State(state): State<AppState>,
    Json(input): Json<ClaimInput>,
) -> Result<Json<Link>, ApiError> {
    Ok(Json(state.store.claim_pairing(&input, unix_now()).await?))
}

async fn create_browser_pairing(
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

async fn client_metadata(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let public_url = state
        .public_url
        .as_deref()
        .ok_or(ApiError::BrowserDisabled)?;
    Ok(Json(oauth::client_metadata(public_url)))
}

async fn link_page(State(state): State<AppState>, Path(token): Path<String>) -> Response {
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
struct LinkForm {
    handle: String,
}

async fn start_browser_link(
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
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn link_callback(
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

fn browser_page(status: StatusCode, title: &str, body: &str) -> Response {
    (
        status,
        Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{}</title></head><body><main><h1>{}</h1>{}</main></body></html>",
            html_escape(title),
            html_escape(title),
            body
        )),
    )
        .into_response()
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

async fn not_found() -> ApiError {
    ApiError::NotFound("route not found".into())
}
