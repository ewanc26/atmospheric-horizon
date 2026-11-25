//! HTTP API for the server-side identity map and browser linking.
//!
//! Public reads let a game client resolve a player UUID. Admin writes are
//! protected by `ATMOSPHERIC_SERVER_TOKEN`; a missing token disables only the
//! admin routes. Allowlisted operators can use a short-lived DID grant (#189)
//! instead of the shared token. Pairing claims are public because possession
//! of the one-time code is the credential. Browser links are created only for
//! the game server, authenticated with `ATMOSPHERIC_SERVER_LINK_TOKEN`, and
//! are then completed by the player's browser OAuth login.

mod browser;
mod health;
mod label;
mod lexicon;
mod link;
mod operator;
mod pairing;
mod skin;

use crate::{
    browser::BrowserLinks,
    error::ApiError,
    labeler::Labeler,
    model::unix_now,
    oauth::{OAuthClient, Resolver},
    operator::{GrantCheck, GrantScope, Operator},
    skin::{SkinSigner, SkinUploads},
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

const LINK_PAGE_LIMIT: usize = 2 * 1024 * 1024;

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
    /// The game worker's Ed25519 endorsement key, when the operator has made
    /// it available for browser skin publishing.
    pub skin_signer: Option<SkinSigner>,
    /// Experimental DID-authenticated operator grants (#189). `None` leaves
    /// only the static admin token available.
    #[doc(hidden)]
    pub operator: Option<Operator>,
}

#[derive(Clone)]
pub struct AppState {
    store: LinkStore,
    browser: BrowserLinks,
    admin_token: Option<Arc<str>>,
    link_token: Option<Arc<str>>,
    public_url: Option<Arc<str>>,
    oauth: Option<OAuthClient>,
    skin_oauth: Option<OAuthClient>,
    skin_uploads: SkinUploads,
    skin_signer: Option<SkinSigner>,
    labeler: Option<Labeler>,
    operator: Option<Operator>,
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
            .map(|url| OAuthClient::new(url, resolver.clone()));
        let skin_oauth = public_url
            .as_deref()
            .map(|url| OAuthClient::with_redirect(url, resolver, "/skin/callback"));
        Self {
            store,
            browser: BrowserLinks::default(),
            admin_token: token(config.admin_token),
            link_token: token(config.link_token),
            public_url,
            oauth,
            skin_oauth,
            skin_uploads: SkinUploads::default(),
            skin_signer: config.skin_signer,
            labeler: config.labeler,
            operator: config.operator,
        }
    }

    fn bearer(headers: &HeaderMap) -> Option<&str> {
        let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        let (scheme, actual) = value.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("Bearer") {
            return None;
        }
        Some(actual.trim())
    }

    /// Authorize an admin operation. The static token remains valid on every
    /// route. A DID grant is accepted only when it carries the route's scope;
    /// both accepted and scope-denied grant uses write signed audit receipts.
    pub(super) async fn authorize_admin(
        &self,
        headers: &HeaderMap,
        scope: GrantScope,
        action: &str,
        target: &str,
    ) -> Result<(), ApiError> {
        let token = Self::bearer(headers);
        if let (Some(operator), Some(token)) = (self.operator.as_ref(), token) {
            match operator.grants.check(token, scope, unix_now()) {
                GrantCheck::Authorized(grant) => {
                    operator
                        .audit
                        .append(&grant, scope.as_str(), action, target, "ok")
                        .await?;
                    return Ok(());
                }
                GrantCheck::ScopeDenied(grant) => {
                    operator
                        .audit
                        .append(&grant, scope.as_str(), action, target, "scope_denied")
                        .await?;
                    return Err(ApiError::Forbidden(format!(
                        "grant does not carry the {scope} scope"
                    )));
                }
                GrantCheck::Unknown => {}
            }
        }
        match (&self.admin_token, token) {
            (Some(expected), Some(actual))
                if constant_time_eq(expected.as_bytes(), actual.as_bytes()) =>
            {
                Ok(())
            }
            (Some(_), _) => Err(ApiError::Unauthorized),
            (None, _) if self.operator.is_some() => Err(ApiError::Unauthorized),
            (None, _) => Err(ApiError::AdminDisabled),
        }
    }

    fn authorize_link(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        if self.link_token.is_none() || self.public_url.is_none() {
            return Err(ApiError::BrowserDisabled);
        }
        let expected = self
            .link_token
            .as_deref()
            .ok_or(ApiError::BrowserDisabled)?;
        match Self::bearer(headers) {
            Some(actual) if constant_time_eq(expected.as_bytes(), actual.as_bytes()) => Ok(()),
            _ => Err(ApiError::Unauthorized),
        }
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
        .route("/v1/labeler/identity", get(label::labeler_identity))
        .route("/v1/operator/challenge", post(operator::challenge))
        .route("/v1/operator/grant", post(operator::grant))
        .route("/v1/operator/receipts", get(operator::receipts))
        .route("/v1/operator/audit-key", get(operator::audit_key))
        .merge(xrpc)
        .route(
            "/v1/browser/pairings",
            post(browser::create_browser_pairing),
        )
        .route("/v1/browser/skins", post(skin::create_skin_upload))
        .route("/oauth-client-metadata.json", get(browser::client_metadata))
        .route("/lexicons", get(lexicon::index))
        .route("/lexicons/{file}", get(lexicon::document))
        .route("/link/callback", get(browser::link_callback))
        .route(
            "/link/{token}",
            get(browser::link_page).post(browser::start_browser_link),
        )
        .route("/skin/callback", get(skin::skin_callback))
        .route(
            "/skin/{token}",
            get(skin::skin_page).post(skin::skin_upload),
        )
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(LINK_PAGE_LIMIT))
        .with_state(AppState::new(store, config))
}

async fn not_found() -> ApiError {
    ApiError::NotFound("route not found".into())
}
