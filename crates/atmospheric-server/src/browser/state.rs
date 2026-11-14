use crate::oauth::PendingAuthorization;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub const BROWSER_TTL_SECS: u64 = 600;

#[derive(Debug, Clone)]
pub struct PendingLink {
    pub player_uuid: String,
    pub player_name: String,
    pub expires_at: u64,
}

struct PendingAuth {
    link_token: String,
    authorization: PendingAuthorization,
    expires_at: u64,
}

#[derive(Default)]
struct State {
    links: BTreeMap<String, PendingLink>,
    auths: BTreeMap<String, PendingAuth>,
}

#[derive(Clone, Default)]
pub struct BrowserLinks {
    state: Arc<Mutex<State>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

impl BrowserLinks {
    pub fn create(&self, player_uuid: &str, player_name: &str) -> (String, u64) {
        let token = crate::oauth::random_token(24);
        let expires_at = now().saturating_add(BROWSER_TTL_SECS);
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.links.insert(
            token.clone(),
            PendingLink {
                player_uuid: player_uuid.to_owned(),
                player_name: player_name.to_owned(),
                expires_at,
            },
        );
        (token, expires_at)
    }

    pub fn pending(&self, token: &str) -> Option<PendingLink> {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.links.get(token).cloned()
    }

    pub fn begin_auth(&self, link_token: &str, authorization: PendingAuthorization) {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.auths.insert(
            authorization.state.clone(),
            PendingAuth {
                link_token: link_token.to_owned(),
                authorization,
                expires_at: now().saturating_add(BROWSER_TTL_SECS),
            },
        );
    }

    pub fn take_auth(&self, auth_state: &str) -> Option<(String, PendingAuthorization)> {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state
            .auths
            .remove(auth_state)
            .map(|pending| (pending.link_token, pending.authorization))
    }

    pub fn take_link(&self, token: &str) -> Option<PendingLink> {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.links.remove(token)
    }
}

fn prune(state: &mut State) {
    let now = now();
    state.links.retain(|_, link| link.expires_at > now);
    state.auths.retain(|_, auth| auth.expires_at > now);
}
