//! HTTP API for the server-side identity map and browser linking.
//!
//! Public reads let a game client resolve a player UUID. Admin writes are
//! protected by `ATMOSPHERIC_SERVER_TOKEN`; a missing token disables only the
//! admin routes. Pairing claims are public because possession of the one-time
//! code is the credential. Browser links are created only for the game server,
//! authenticated with `ATMOSPHERIC_SERVER_LINK_TOKEN`, and are then completed
//! by the player's browser OAuth login.

mod browser;
mod health;
mod label;
mod lexicon;
mod link;
mod pairing;

use crate::{
    browser::BrowserLinks,
    error::ApiError,
    labeler::Labeler,
    oauth::{OAuthClient, Resolver},
    store::LinkStore,
};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderMap, HeaderName, Method, header},
    routing::{get, post},
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

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
    /// The labeler service, when `ATMOSPHERIC_SERVER_LABELER_DID` is set.
    pub labeler: Option<Labeler>,
}

#[derive(Clone)]
pub struct AppState {
    store: LinkStore,
    browser: BrowserLinks,
    admin_token: Option<Arc<str>>,
    link_token: Option<Arc<str>>,
    public_url: Option<Arc<str>>,
    oauth: Option<OAuthClient>,
    labeler: Option<Labeler>,
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
            labeler: config.labeler,
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
    // The labeler's XRPC reads are public and fetched cross-origin by browser
    // clients (Bluesky, Ozone, dashboards), so they must answer permissive CORS.
    // The WebSocket upgrade is not subject to preflight, but shares the lane.
    let xrpc = Router::new()
        .route(
            "/xrpc/com.atproto.label.queryLabels",
            get(label::query_labels),
        )
        .route(
            "/xrpc/com.atproto.label.subscribeLabels",
            get(label::subscribe_labels),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(vec![Method::GET, Method::OPTIONS])
                .allow_headers(vec![
                    header::AUTHORIZATION,
                    header::CONTENT_TYPE,
                    HeaderName::from_static("atproto-accept-labelers"),
                    HeaderName::from_static("atproto-proxy"),
                ]),
        );

    Router::new()
        .route("/healthz", get(health::health))
        .route("/v1/links", get(link::list_links))
        .route(
            "/v1/links/{uuid}",
            get(link::get_link)
                .put(link::put_link)
                .delete(link::delete_link),
        )
        .route("/v1/pairings", post(pairing::create_pairing))
        .route("/v1/pairings/claim", post(pairing::claim_pairing))
        .route("/v1/labeler/labels", post(label::emit_label))
        .merge(xrpc)
        .route(
            "/v1/browser/pairings",
            post(browser::create_browser_pairing),
        )
        .route("/oauth-client-metadata.json", get(browser::client_metadata))
        .route("/lexicons", get(lexicon::index))
        .route("/lexicons/{file}", get(lexicon::document))
        .route("/link/callback", get(browser::link_callback))
        .route(
            "/link/{token}",
            get(browser::link_page).post(browser::start_browser_link),
        )
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(LINK_PAGE_LIMIT))
        .with_state(AppState::new(store, config))
}

async fn not_found() -> ApiError {
    ApiError::NotFound("route not found".into())
}
