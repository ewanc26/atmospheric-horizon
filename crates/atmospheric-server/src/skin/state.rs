use crate::oauth::PendingAuthorization;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// Lifetime of one browser skin link. The OAuth attempt has the same bound.
pub const SKIN_UPLOAD_TTL_SECS: u64 = 600;

#[derive(Debug, Clone)]
pub struct PendingSkin {
    pub player_uuid: String,
    pub player_name: String,
    /// The linked DID this upload is allowed to write as.
    pub did: String,
    pub handle: Option<String>,
    pub expires_at: u64,
}

#[derive(Clone)]
pub struct PendingSkinAuth {
    pub token: String,
    pub model: String,
    pub bytes: Vec<u8>,
    pub authorization: PendingAuthorization,
    pub expires_at: u64,
}

#[derive(Default)]
struct State {
    uploads: BTreeMap<String, PendingSkin>,
    auths: BTreeMap<String, PendingSkinAuth>,
}

#[derive(Clone, Default)]
pub struct SkinUploads {
    state: Arc<Mutex<State>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

impl SkinUploads {
    pub fn create(
        &self,
        player_uuid: &str,
        player_name: &str,
        did: &str,
        handle: Option<&str>,
        ttl_secs: u64,
    ) -> (String, u64) {
        let token = crate::oauth::random_token(24);
        let expires_at = now().saturating_add(ttl_secs);
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.uploads.insert(
            token.clone(),
            PendingSkin {
                player_uuid: player_uuid.to_owned(),
                player_name: player_name.to_owned(),
                did: did.to_owned(),
                handle: handle.map(str::to_owned),
                expires_at,
            },
        );
        (token, expires_at)
    }

    pub fn pending(&self, token: &str) -> Option<PendingSkin> {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.uploads.get(token).cloned()
    }

    pub fn begin_auth(
        &self,
        token: &str,
        model: &str,
        bytes: Vec<u8>,
        authorization: PendingAuthorization,
    ) {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.auths.insert(
            authorization.state.clone(),
            PendingSkinAuth {
                token: token.to_owned(),
                model: model.to_owned(),
                bytes,
                authorization,
                expires_at: now().saturating_add(SKIN_UPLOAD_TTL_SECS),
            },
        );
    }

    pub fn take_auth(&self, auth_state: &str) -> Option<PendingSkinAuth> {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.auths.remove(auth_state)
    }

    pub fn consume(&self, token: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        prune(&mut state);
        state.uploads.remove(token).is_some()
    }
}

fn prune(state: &mut State) {
    let now = now();
    state.uploads.retain(|_, upload| upload.expires_at > now);
    state.auths.retain(|_, auth| auth.expires_at > now);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authorization() -> PendingAuthorization {
        PendingAuthorization {
            state: "state-token".into(),
            verifier: "verifier".into(),
            key: crate::oauth::DpopKey::generate(),
            token_endpoint: "https://auth.example/token".into(),
            issuer: "https://auth.example".into(),
            did: "did:plc:alice".into(),
            handle: "alice.test".into(),
            pds: "https://pds.example".into(),
        }
    }

    #[test]
    fn upload_tokens_are_one_time_and_bound_to_the_linked_did() {
        let uploads = SkinUploads::default();
        let (token, expires_at) = uploads.create(
            "00000000-0000-4000-8000-000000000001",
            "Alex",
            "did:plc:alice",
            Some("alice.test"),
            300,
        );
        let pending = uploads.pending(&token).unwrap();
        assert_eq!(pending.did, "did:plc:alice");
        assert_eq!(pending.handle.as_deref(), Some("alice.test"));
        assert!(expires_at > 0);
        assert!(uploads.consume(&token));
        assert!(uploads.pending(&token).is_none());
        assert!(!uploads.consume(&token));
    }

    #[test]
    fn pending_auth_keeps_the_bytes_and_is_consumed_once() {
        let uploads = SkinUploads::default();
        let (token, _) = uploads.create(
            "00000000-0000-4000-8000-000000000001",
            "Alex",
            "did:plc:alice",
            None,
            300,
        );
        uploads.begin_auth(&token, "slim", vec![1, 2, 3], authorization());
        let auth = uploads.take_auth("state-token").unwrap();
        assert_eq!(auth.model, "slim");
        assert_eq!(auth.bytes, vec![1, 2, 3]);
        assert!(uploads.take_auth("state-token").is_none());
    }
}
