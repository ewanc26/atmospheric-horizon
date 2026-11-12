//! App-password login and session state.

use crate::Config;
use atrium_api::agent::atp_agent::{store::MemorySessionStore, AtpAgent};
use atrium_xrpc_client::reqwest::ReqwestClient;

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

/// Build an agent for the configured PDS.
pub fn agent_for(config: &Config) -> Agent {
    AtpAgent::new(
        ReqwestClient::new(config.pds_url.clone()),
        MemorySessionStore::default(),
    )
}

/// Log in with the app-password flow (`com.atproto.server.createSession`).
///
/// The session stays in the agent's in-memory store for the worker's lifetime;
/// restarting the game re-authenticates. Persisting the session across
/// restarts is an M3 concern (a `FileStore`-style store under the mod's
/// storage folder, never world saves).
pub async fn login(config: &Config) -> crate::error::Result<Agent> {
    let agent = agent_for(config);
    agent
        .login(&config.identifier, &config.app_password)
        .await
        .map_err(crate::error::AtmosphericError::protocol)?;
    Ok(agent)
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
}
