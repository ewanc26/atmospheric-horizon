//! App-password login and persisted session state.
//!
//! The session lives in the agent's in-memory store for the worker's
//! lifetime; the refresh token is persisted to `session.json` under the
//! mod's own storage folder (never world saves) so a restart can resume via
//! `com.atproto.server.refreshSession` instead of needing the app password
//! again. Stored at rest unencrypted — see the README's Network use section.

use crate::Config;
use crate::error::{AtmosphericError, Result};
use atrium_api::agent::atp_agent::{AtpAgent, store::MemorySessionStore};
use atrium_xrpc_client::reqwest::ReqwestClient;
use serde::{Deserialize, Serialize};

/// The agent type used throughout the mod.
///
/// `AtpAgent<S, T>` takes the session store first and the XRPC client second.
pub type Agent = AtpAgent<MemorySessionStore, ReqwestClient>;

/// Session health, mirrored on the HUD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    LoggedOut,
    LoggingIn,
    LoggedIn { did: String },
    Failed { message: String },
}

/// The persisted part of a session: enough to resume it after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    /// The PDS the session belongs to.
    pub pds_url: String,
    pub did: String,
    pub handle: String,
    /// The refresh token; app passwords are never stored.
    pub refresh_jwt: String,
}

/// Build an agent for a PDS URL.
pub fn agent_for_pds(pds_url: &str) -> Agent {
    AtpAgent::new(ReqwestClient::new(pds_url), MemorySessionStore::default())
}

/// Build an agent for the configured PDS.
pub fn agent_for(config: &Config) -> Agent {
    agent_for_pds(&config.pds_url)
}

/// Log in with the app-password flow (`com.atproto.server.createSession`).
///
/// Returns the agent plus the session to persist.
pub async fn login(config: &Config) -> Result<(Agent, StoredSession)> {
    let agent = agent_for(config);
    let session = agent
        .login(&config.identifier, &config.app_password)
        .await
        .map_err(AtmosphericError::protocol)?;
    let stored = StoredSession {
        pds_url: config.pds_url.clone(),
        did: session.data.did.as_str().to_owned(),
        handle: session.data.handle.as_str().to_owned(),
        refresh_jwt: session.data.refresh_jwt.clone(),
    };
    Ok((agent, stored))
}

/// Resume a persisted session (`com.atproto.server.refreshSession` with the
/// stored refresh token, then `getSession` via `resume_session`).
///
/// The refresh token rotates on every call, so the returned `StoredSession`
/// is the one to persist again.
pub async fn resume(stored: &StoredSession) -> Result<(Agent, StoredSession)> {
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
    let agent = agent_for_pds(&stored.pds_url);
    agent
        .resume_session(output.clone())
        .await
        .map_err(AtmosphericError::protocol)?;
    let stored = StoredSession {
        pds_url: stored.pds_url.clone(),
        did: output.data.did.as_str().to_owned(),
        handle: output.data.handle.as_str().to_owned(),
        refresh_jwt: output.data.refresh_jwt.clone(),
    };
    Ok((agent, stored))
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
        };
        let raw = serde_json::to_string(&stored).unwrap();
        let back: StoredSession = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.did, stored.did);
        assert_eq!(back.refresh_jwt, stored.refresh_jwt);
        // No app password anywhere in the persisted form.
        assert!(!raw.contains("password"));
    }
}
