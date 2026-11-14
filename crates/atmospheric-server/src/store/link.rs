use super::file::{DiskState, STORE_VERSION, persist};
use crate::{
    error::ApiError,
    model::{
        ClaimInput, Link, LinkInput, PairingInput, PairingResponse, new_pairing_code,
        normalize_code, normalize_did, normalize_handle, normalize_player_name, normalize_uuid,
        pairing_ttl,
    },
};
use std::{fs, path::PathBuf, sync::Arc};
use tokio::sync::RwLock;

/// A cheap-to-clone handle to the shared store.
#[derive(Clone)]
pub struct LinkStore {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    state: RwLock<DiskState>,
}

impl LinkStore {
    /// Open (or create in memory) the store at `path`. A malformed or
    /// unsupported file is a startup error rather than silent data loss.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ApiError> {
        let path = path.into();
        let state = match fs::read(&path) {
            Ok(bytes) => {
                let state: DiskState = serde_json::from_slice(&bytes)
                    .map_err(|error| ApiError::Corrupt(error.to_string()))?;
                if state.version != STORE_VERSION {
                    return Err(ApiError::Corrupt(format!(
                        "unsupported store version {} (expected {STORE_VERSION})",
                        state.version
                    )));
                }
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DiskState::empty(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                state: RwLock::new(state),
            }),
        })
    }

    /// Resolve a player UUID.
    pub async fn link(&self, uuid: &str) -> Result<Option<Link>, ApiError> {
        let uuid = normalize_uuid(uuid)?;
        Ok(self.inner.state.read().await.links.get(&uuid).cloned())
    }

    /// All links, ordered by player name then UUID.
    pub async fn links(&self) -> Vec<Link> {
        let state = self.inner.state.read().await;
        let mut links: Vec<Link> = state.links.values().cloned().collect();
        links.sort_by(|left, right| {
            left.player_name
                .to_ascii_lowercase()
                .cmp(&right.player_name.to_ascii_lowercase())
                .then_with(|| left.player_uuid.cmp(&right.player_uuid))
        });
        links
    }

    /// Counts used by the health endpoint. Expired pairings are not counted,
    /// but they are removed on the next mutation.
    pub async fn counts(&self, now: u64) -> (usize, usize) {
        let state = self.inner.state.read().await;
        let pending = state
            .pairings
            .values()
            .filter(|pairing| pairing.expires_at > now)
            .count();
        (state.links.len(), pending)
    }

    /// Create or replace a link. The original `linked_at` is preserved when a
    /// player is relinked. Returns the link and whether it already existed.
    pub async fn upsert_link(
        &self,
        uuid: &str,
        input: &LinkInput,
        now: u64,
    ) -> Result<(Link, bool), ApiError> {
        let uuid = normalize_uuid(uuid)?;
        let player_name = normalize_player_name(&input.player_name)?;
        let did = normalize_did(&input.did)?;
        let handle = normalize_handle(input.handle.as_deref())?;
        self.mutate(|state| {
            let existed = state.links.contains_key(&uuid);
            let linked_at = state
                .links
                .get(&uuid)
                .map(|link| link.linked_at)
                .unwrap_or(now);
            let link = Link {
                player_uuid: uuid.clone(),
                player_name,
                did,
                handle,
                linked_at,
            };
            state.links.insert(uuid, link.clone());
            Ok((link, existed))
        })
        .await
    }

    /// Remove a link. Returns whether it existed.
    pub async fn remove_link(&self, uuid: &str) -> Result<bool, ApiError> {
        let uuid = normalize_uuid(uuid)?;
        self.mutate(|state| Ok(state.links.remove(&uuid).is_some()))
            .await
    }

    /// Issue a one-time pairing code, replacing any earlier pending code for
    /// the same player.
    pub async fn create_pairing(
        &self,
        input: &PairingInput,
        now: u64,
    ) -> Result<PairingResponse, ApiError> {
        let player_uuid = normalize_uuid(&input.player_uuid)?;
        let player_name = normalize_player_name(&input.player_name)?;
        let ttl = pairing_ttl(input.ttl_seconds)?;
        self.mutate(|state| {
            state.pairings.retain(|_, pairing| pairing.expires_at > now);
            state
                .pairings
                .retain(|_, pairing| pairing.player_uuid != player_uuid);
            for _ in 0..16 {
                let code = new_pairing_code();
                // Store the canonical code (no hyphen) so a claim normalised
                // by `normalize_code` finds the same key. The response keeps
                // the readable hyphenated form.
                let key = normalize_code(&code).expect("generated pairing code is valid");
                if state.pairings.contains_key(&key) {
                    continue;
                }
                let expires_at = now.saturating_add(ttl);
                state.pairings.insert(
                    key,
                    super::file::PendingPairing {
                        player_uuid: player_uuid.clone(),
                        player_name: player_name.clone(),
                        expires_at,
                    },
                );
                return Ok(PairingResponse { code, expires_at });
            }
            Err(ApiError::Internal)
        })
        .await
    }

    /// Consume a pairing code and bind the player to the claimed DID. The code
    /// is removed only after the new state is durably written.
    pub async fn claim_pairing(&self, input: &ClaimInput, now: u64) -> Result<Link, ApiError> {
        let code = normalize_code(&input.code)?;
        let did = normalize_did(&input.did)?;
        let handle = normalize_handle(input.handle.as_deref())?;
        self.mutate(|state| {
            state.pairings.retain(|_, pairing| pairing.expires_at > now);
            let pending = state
                .pairings
                .remove(&code)
                .ok_or(ApiError::PairingUnavailable)?;
            let link = Link {
                player_uuid: pending.player_uuid.clone(),
                player_name: pending.player_name,
                did,
                handle,
                linked_at: now,
            };
            state.links.insert(pending.player_uuid, link.clone());
            Ok(link)
        })
        .await
    }

    async fn mutate<T>(
        &self,
        change: impl FnOnce(&mut DiskState) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut state = self.inner.state.write().await;
        let mut next = state.clone();
        let value = change(&mut next)?;
        persist(self.inner.path.clone(), next.clone()).await?;
        *state = next;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::default_links_path;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        default_links_path(std::env::temp_dir().join(format!(
            "atmospheric-server-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn link_input(did: &str) -> LinkInput {
        LinkInput {
            player_name: "Alex".into(),
            did: did.into(),
            handle: Some("alex.test".into()),
        }
    }

    #[tokio::test]
    async fn links_survive_reopen_and_preserve_linked_at() {
        let path = temp_path("reopen");
        let store = LinkStore::open(&path).unwrap();
        let first = store
            .upsert_link(
                "00000000-0000-4000-8000-000000000001",
                &link_input("did:plc:alice"),
                100,
            )
            .await
            .unwrap();
        assert!(!first.1);
        let second = store
            .upsert_link(
                "00000000-0000-4000-8000-000000000001",
                &link_input("did:plc:alice2"),
                200,
            )
            .await
            .unwrap();
        assert!(second.1);
        assert_eq!(second.0.linked_at, 100);
        drop(store);

        let reopened = LinkStore::open(&path).unwrap();
        let link = reopened
            .link("00000000000040008000000000000001")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(link.did, "did:plc:alice2");
        assert_eq!(link.linked_at, 100);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn pairing_claims_once_and_expires() {
        let path = temp_path("pairing");
        let store = LinkStore::open(&path).unwrap();
        let pairing = store
            .create_pairing(
                &PairingInput {
                    player_uuid: "00000000-0000-4000-8000-000000000002".into(),
                    player_name: "Steve".into(),
                    ttl_seconds: Some(60),
                },
                1_000,
            )
            .await
            .unwrap();
        assert_eq!(pairing.expires_at, 1_060);

        let claimed = store
            .claim_pairing(
                &ClaimInput {
                    code: pairing.code.to_lowercase(),
                    did: "did:plc:bob".into(),
                    handle: Some("Bob.Test".into()),
                },
                1_001,
            )
            .await
            .unwrap();
        assert_eq!(claimed.player_name, "Steve");
        assert_eq!(claimed.handle.as_deref(), Some("bob.test"));
        assert!(matches!(
            store
                .claim_pairing(
                    &ClaimInput {
                        code: pairing.code,
                        did: "did:plc:other".into(),
                        handle: None,
                    },
                    1_002,
                )
                .await,
            Err(ApiError::PairingUnavailable)
        ));

        let expired = store
            .create_pairing(
                &PairingInput {
                    player_uuid: "00000000-0000-4000-8000-000000000003".into(),
                    player_name: "Herobrine".into(),
                    ttl_seconds: Some(60),
                },
                2_000,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .claim_pairing(
                    &ClaimInput {
                        code: expired.code,
                        did: "did:plc:late".into(),
                        handle: None,
                    },
                    2_061,
                )
                .await,
            Err(ApiError::PairingUnavailable)
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn removing_a_link_is_idempotent() {
        let path = temp_path("remove");
        let store = LinkStore::open(&path).unwrap();
        store
            .upsert_link(
                "00000000-0000-4000-8000-000000000004",
                &link_input("did:plc:alice"),
                1,
            )
            .await
            .unwrap();
        assert!(
            store
                .remove_link("00000000-0000-4000-8000-000000000004")
                .await
                .unwrap()
        );
        assert!(
            !store
                .remove_link("00000000-0000-4000-8000-000000000004")
                .await
                .unwrap()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
