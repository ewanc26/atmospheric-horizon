//! Login flows (app password and OAuth) and the agent plumbing.
//!
//! The session lives in the agent's in-memory store for the worker's
//! lifetime; the refresh token is persisted to `session.json` under the
//! mod's own storage folder (never world saves) so a restart can resume
//! without the credentials again. For OAuth logins the persisted file also
//! carries the DPoP private key (JWK) — secret material, stored at rest
//! unencrypted; see the README's Network use section.

use crate::{
    Config,
    error::{AtmosphericError, Result},
    oauth::{self, DpopKey, DpopState, SharedDpop, TokenSet},
};
use atrium_api::agent::{
    AuthorizationProvider,
    atp_agent::{AtpAgent, AtpSession, store::AtpSessionStore},
};
use atrium_common::store::Store;
use atrium_xrpc::{
    HttpClient, XrpcClient,
    http::{self, Request, Response},
    types::AuthorizationToken,
};
use atrium_xrpc_client::reqwest::ReqwestClient;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// The agent type used throughout the mod: a session store that answers with
/// `Bearer` (app-password sessions) or `DPoP` (OAuth sessions) tokens, over an
/// XRPC client that adds a DPoP proof header to every request in OAuth mode.
pub type Agent = AtpAgent<SessionStore, DpopClient>;

/// Session health, mirrored on the HUD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    LoggedOut,
    LoggingIn,
    LoggedIn { did: String },
    Failed { message: String },
}

/// The agent's session store. In OAuth mode the authorization token comes from
/// the shared [`SharedDpop`] state as `AuthorizationToken::Dpop`; otherwise it
/// is the session's JWT as a `Bearer` token, exactly as before.
pub struct SessionStore {
    session: Mutex<Option<AtpSession>>,
    pub dpop: SharedDpop,
}

impl Store<(), AtpSession> for SessionStore {
    type Error = std::convert::Infallible;

    async fn get(&self, _key: &()) -> std::result::Result<Option<AtpSession>, Self::Error> {
        Ok(self.session.lock().unwrap().clone())
    }

    async fn set(&self, _key: (), value: AtpSession) -> std::result::Result<(), Self::Error> {
        *self.session.lock().unwrap() = Some(value);
        Ok(())
    }

    async fn del(&self, _key: &()) -> std::result::Result<(), Self::Error> {
        *self.session.lock().unwrap() = None;
        Ok(())
    }

    async fn clear(&self) -> std::result::Result<(), Self::Error> {
        *self.session.lock().unwrap() = None;
        Ok(())
    }
}

impl AuthorizationProvider for SessionStore {
    async fn authorization_token(&self, is_refresh: bool) -> Option<AuthorizationToken> {
        if let Some(state) = self.dpop.lock().unwrap().as_ref() {
            let token = if is_refresh {
                &state.refresh_token
            } else {
                &state.access_token
            };
            return Some(AuthorizationToken::Dpop(token.clone()));
        }
        self.session.lock().unwrap().as_ref().map(|session| {
            let data = &session.data;
            let token = if is_refresh {
                &data.refresh_jwt
            } else {
                &data.access_jwt
            };
            AuthorizationToken::Bearer(token.clone())
        })
    }
}

impl AtpSessionStore for SessionStore {}

/// The XRPC client: reqwest underneath. While DPoP state is installed, every
/// request gets a `DPoP` proof header (htm/htu from the request, `ath` from its
/// own Authorization header), retrying with the server's `DPoP-Nonce` when it
/// answers 400/401 with one. In app-password mode it is a plain pass-through.
#[derive(Clone)]
pub struct DpopClient {
    inner: ReqwestClient,
    dpop: SharedDpop,
}

impl HttpClient for DpopClient {
    async fn send_http(
        &self,
        request: Request<Vec<u8>>,
    ) -> std::result::Result<Response<Vec<u8>>, Box<dyn std::error::Error + Send + Sync + 'static>>
    {
        let state = self.dpop.lock().unwrap().clone();
        let Some(state) = state else {
            return self.inner.send_http(request).await;
        };
        let htm = request.method().as_str().to_owned();
        let htu = oauth::normalize_htu(request.uri());
        let ath = request
            .headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("DPoP "))
            .map(oauth::access_token_hash);
        let dpop_header = http::HeaderName::from_bytes(b"DPoP").expect("valid header name");
        let mut request = request;
        let mut nonce: Option<String> = None;
        for _attempt in 0..3 {
            let proof = state
                .key
                .proof(&htm, &htu, nonce.as_deref(), ath.as_deref())?;
            request
                .headers_mut()
                .insert(dpop_header.clone(), http::HeaderValue::from_str(&proof)?);
            let response = self.inner.send_http(request.clone()).await?;
            let challenged = matches!(response.status().as_u16(), 400 | 401)
                && response.headers().contains_key("DPoP-Nonce");
            if challenged {
                nonce = response
                    .headers()
                    .get("DPoP-Nonce")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                continue;
            }
            return Ok(response);
        }
        Err("the PDS kept challenging the DPoP nonce".into())
    }
}

impl XrpcClient for DpopClient {
    fn base_uri(&self) -> String {
        self.inner.base_uri()
    }
}

/// The persisted part of a session: enough to resume it after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    /// The PDS the session belongs to.
    pub pds_url: String,
    pub did: String,
    pub handle: String,
    /// The refresh token; app passwords are never stored. For OAuth sessions
    /// this is the DPoP-bound OAuth refresh token.
    pub refresh_jwt: String,
    /// The OAuth material, when the session came from a localhost OAuth login.
    #[serde(default)]
    pub oauth: Option<StoredOAuth>,
}

/// Persisted OAuth material: the DPoP private key (secret — never logged) and
/// the authorization server endpoints needed to refresh the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOAuth {
    pub dpop_jwk: String,
    pub token_endpoint: String,
    pub issuer: String,
    /// The client_id the session was authorized with (loopback default or a
    /// hosted metadata URL); the refresh grant must use the same one.
    pub client_id: String,
    pub scope: String,
}

/// Build an agent for a PDS URL with the given shared DPoP state.
pub fn agent_for_pds_with(pds_url: &str, dpop: SharedDpop) -> Agent {
    let store = SessionStore {
        session: Mutex::new(None),
        dpop: dpop.clone(),
    };
    let client = DpopClient {
        inner: ReqwestClient::new(pds_url),
        dpop,
    };
    AtpAgent::new(client, store)
}

/// Build an agent for a PDS URL (no DPoP state: app-password mode).
pub fn agent_for_pds(pds_url: &str) -> Agent {
    agent_for_pds_with(pds_url, oauth::new_shared_dpop())
}

/// Build an agent for the configured PDS.
pub fn agent_for(config: &Config) -> Agent {
    agent_for_pds(&config.pds_url)
}

/// Log in with the app-password flow (`com.atproto.server.createSession`).
///
/// Returns the agent plus the session to persist.
pub async fn login(config: &Config, dpop: SharedDpop) -> Result<(Agent, StoredSession)> {
    let agent = agent_for_pds_with(&config.pds_url, dpop);
    let session = agent
        .login(&config.identifier, &config.app_password)
        .await
        .map_err(AtmosphericError::protocol)?;
    let stored = StoredSession {
        pds_url: config.pds_url.clone(),
        did: session.data.did.as_str().to_owned(),
        handle: session.data.handle.as_str().to_owned(),
        refresh_jwt: session.data.refresh_jwt.clone(),
        oauth: None,
    };
    Ok((agent, stored))
}

/// Resume a persisted app-password session (`com.atproto.server.refreshSession`
/// with the stored refresh token, then `getSession` via `resume_session`).
///
/// The refresh token rotates on every call, so the returned `StoredSession`
/// is the one to persist again.
pub async fn resume(stored: &StoredSession, dpop: SharedDpop) -> Result<(Agent, StoredSession)> {
    // The refresh endpoint takes the refresh token as the bearer token, and
    // the agent cannot seed its store without a full session, so this one
    // call goes through reqwest directly.
    let response = reqwest::Client::new()
        .post(format!(
            "{}/xrpc/com.atproto.server.refreshSession",
            stored.pds_url.trim_end_matches('/')
        ))
        .bearer_auth(&stored.refresh_jwt)
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    if !response.status().is_success() {
        return Err(AtmosphericError::Message(format!(
            "refreshSession failed with HTTP {}",
            response.status()
        )));
    }
    // (reqwest's `json` feature is off; parse the body ourselves.)
    let body = response.text().await.map_err(AtmosphericError::protocol)?;
    let output: atrium_api::com::atproto::server::create_session::Output =
        serde_json::from_str(&body)?;
    let agent = agent_for_pds_with(&stored.pds_url, dpop);
    agent
        .resume_session(output.clone())
        .await
        .map_err(AtmosphericError::protocol)?;
    let stored = StoredSession {
        pds_url: stored.pds_url.clone(),
        did: output.data.did.as_str().to_owned(),
        handle: output.data.handle.as_str().to_owned(),
        refresh_jwt: output.data.refresh_jwt.clone(),
        oauth: None,
    };
    Ok((agent, stored))
}

/// Build an OAuth agent: DPoP state installed (so every request is signed and
/// authorised as `DPoP`), the store seeded with the tokens, and the session
/// validated via `getSession` — which itself goes out DPoP-signed.
///
/// Returns the agent plus the shared DPoP state the worker keeps for refreshes.
pub async fn agent_for_oauth(
    pds_url: &str,
    did: &str,
    handle: &str,
    tokens: &TokenSet,
    key: DpopKey,
) -> Result<(Agent, SharedDpop)> {
    let state = DpopState::from_tokens(key, tokens);
    let dpop: SharedDpop = std::sync::Arc::new(Mutex::new(Some(state)));
    let agent = agent_for_pds_with(pds_url, dpop.clone());
    let output = atrium_api::com::atproto::server::create_session::Output::from(
        atrium_api::com::atproto::server::create_session::OutputData {
            access_jwt: tokens.access_token.clone(),
            active: Some(true),
            did: did
                .parse()
                .map_err(|_| AtmosphericError::Message(format!("invalid DID: {did}")))?,
            did_doc: None,
            email: None,
            email_auth_factor: None,
            email_confirmed: None,
            handle: handle
                .parse()
                .map_err(|_| AtmosphericError::Message(format!("invalid handle: {handle}")))?,
            refresh_jwt: tokens.refresh_token.clone().unwrap_or_default(),
            status: None,
        },
    );
    agent
        .resume_session(output)
        .await
        .map_err(AtmosphericError::protocol)?;
    Ok((agent, dpop))
}

/// Resume a persisted OAuth session: refresh the DPoP-bound token at the
/// authorization server, then build the agent exactly as after a fresh login.
pub async fn resume_oauth(
    http: &reqwest::Client,
    stored: &StoredSession,
    oauth: &StoredOAuth,
) -> Result<(Agent, SharedDpop, StoredSession)> {
    let key = DpopKey::from_jwk_json(&oauth.dpop_jwk)?;
    let tokens = oauth::refresh(
        http,
        &oauth.token_endpoint,
        &oauth.client_id,
        &stored.refresh_jwt,
        &key,
    )
    .await?;
    let did = stored.did.clone();
    let handle = stored.handle.clone();
    let (agent, dpop) = agent_for_oauth(&stored.pds_url, &did, &handle, &tokens, key).await?;
    let rotated = StoredSession {
        pds_url: stored.pds_url.clone(),
        did,
        handle,
        refresh_jwt: tokens
            .refresh_token
            .clone()
            .unwrap_or_else(|| stored.refresh_jwt.clone()),
        oauth: Some(oauth.clone()),
    };
    Ok((agent, dpop, rotated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_is_built_for_the_configured_pds() {
        // No network: building the agent only constructs the client.
        let config = Config {
            pds_url: "https://pds.example".into(),
            ..Config::default()
        };
        let _agent = agent_for(&config);
    }

    #[test]
    fn stored_session_round_trips_through_json() {
        let stored = StoredSession {
            pds_url: "https://pds.example".into(),
            did: "did:plc:alice".into(),
            handle: "alice.test".into(),
            refresh_jwt: "refresh-token".into(),
            oauth: None,
        };
        let raw = serde_json::to_string(&stored).unwrap();
        let back: StoredSession = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.did, stored.did);
        assert_eq!(back.refresh_jwt, stored.refresh_jwt);
        assert!(back.oauth.is_none());
        // No app password anywhere in the persisted form.
        assert!(!raw.contains("password"));
    }

    #[test]
    fn stored_oauth_session_round_trips_and_marks_the_flow() {
        let original = DpopKey::generate();
        let stored = StoredSession {
            pds_url: "https://pds.example".into(),
            did: "did:plc:alice".into(),
            handle: "alice.test".into(),
            refresh_jwt: "oauth-refresh-token".into(),
            oauth: Some(StoredOAuth {
                dpop_jwk: original.to_jwk_json(),
                token_endpoint: "https://auth.example/token".into(),
                issuer: "https://auth.example".into(),
                client_id: oauth::client_id(oauth::DEFAULT_SCOPE),
                scope: oauth::DEFAULT_SCOPE.into(),
            }),
        };
        let raw = serde_json::to_string(&stored).unwrap();
        let back: StoredSession = serde_json::from_str(&raw).unwrap();
        let oauth = back.oauth.expect("oauth block survives");
        // The persisted DPoP key round-trips into the same key (same thumbprint).
        let key = DpopKey::from_jwk_json(&oauth.dpop_jwk).unwrap();
        assert_eq!(key.thumbprint(), original.thumbprint());
        assert!(raw.contains("dpop_jwk"));
    }

    #[tokio::test]
    async fn authorization_token_is_bearer_without_dpop_and_dpop_with() {
        let dpop = oauth::new_shared_dpop();
        let store = SessionStore {
            session: Mutex::new(None),
            dpop: dpop.clone(),
        };
        // No session at all: no token.
        assert!(store.authorization_token(false).await.is_none());
        // With DPoP state: DPoP access/refresh tokens.
        *dpop.lock().unwrap() = Some(DpopState {
            key: DpopKey::generate(),
            access_token: "access-1".into(),
            refresh_token: "refresh-1".into(),
            expires_at: None,
            scope: oauth::DEFAULT_SCOPE.into(),
        });
        match store.authorization_token(false).await {
            Some(AuthorizationToken::Dpop(token)) => assert_eq!(token, "access-1"),
            _ => panic!("expected Dpop(access)"),
        }
        match store.authorization_token(true).await {
            Some(AuthorizationToken::Dpop(token)) => assert_eq!(token, "refresh-1"),
            _ => panic!("expected Dpop(refresh)"),
        }
    }
}
