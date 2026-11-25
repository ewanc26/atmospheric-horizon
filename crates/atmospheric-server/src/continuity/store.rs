//! In-memory continuity state. Nothing here is persisted: a restart starts a
//! fresh watch list and lets the feed/poll rebuild observations from PLC.

use super::model::{
    ContinuitySettings, ContinuityStatus, DEFAULT_MAX_EVENTS, ExportOperation, IdentityEvent,
    IdentitySnapshot, WatchedIdentity, diff_snapshots, snapshot_from_operation, validate_watch_did,
};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::RwLock;

/// A cheaply cloned handle shared by the HTTP handlers and the watcher tasks.
#[derive(Clone, Debug)]
pub struct Continuity {
    inner: Arc<RwLock<State>>,
    settings: ContinuitySettings,
    max_events: usize,
}

#[derive(Debug, Default)]
struct State {
    watched: std::collections::BTreeMap<String, Option<String>>,
    snapshots: std::collections::BTreeMap<String, IdentitySnapshot>,
    events: VecDeque<IdentityEvent>,
    next_event_id: u64,
    cursor: Option<u64>,
}

impl Continuity {
    /// Create an empty watcher state with the given startup policy and event
    /// cap. Tests can use a small cap to bound their fixtures.
    pub fn new(settings: ContinuitySettings, max_events: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(State::default())),
            settings,
            max_events: max_events.max(1),
        }
    }

    pub fn with_settings(settings: ContinuitySettings) -> Self {
        Self::new(settings, DEFAULT_MAX_EVENTS)
    }

    /// Add a DID to the watch list. The UUID comes only from the existing link
    /// map; a stale UUID is replaced when the game server re-registers it.
    pub async fn watch(&self, did: &str, uuid: Option<String>) -> Result<(), String> {
        let did = validate_watch_did(did)?;
        let mut state = self.inner.write().await;
        state.watched.insert(did.clone(), uuid.clone());
        if let Some(snapshot) = state.snapshots.get_mut(&did)
            && uuid.is_some()
        {
            snapshot.uuid = uuid;
        }
        Ok(())
    }

    /// Stop watching a DID. The last snapshot and events remain readable so a
    /// game server can drain them after a player leaves.
    pub async fn unwatch(&self, did: &str) {
        let did = did.trim().to_owned();
        self.inner.write().await.watched.remove(&did);
    }

    pub async fn watched(&self) -> Vec<WatchedIdentity> {
        let state = self.inner.read().await;
        state
            .watched
            .iter()
            .map(|(did, uuid)| WatchedIdentity {
                did: did.clone(),
                uuid: uuid.clone(),
                has_snapshot: state.snapshots.contains_key(did),
            })
            .collect()
    }

    pub async fn snapshot(&self, did: &str) -> Option<IdentitySnapshot> {
        self.inner.read().await.snapshots.get(did.trim()).cloned()
    }

    pub async fn snapshots(&self) -> Vec<IdentitySnapshot> {
        let state = self.inner.read().await;
        state
            .snapshots
            .values()
            .filter(|snapshot| state.watched.contains_key(&snapshot.did))
            .cloned()
            .collect()
    }

    pub async fn events(&self, limit: usize) -> Vec<IdentityEvent> {
        let state = self.inner.read().await;
        let limit = limit.clamp(1, self.max_events);
        state.events.iter().rev().take(limit).cloned().collect()
    }

    pub async fn status(&self) -> ContinuityStatus {
        let state = self.inner.read().await;
        ContinuityStatus {
            watched: state.watched.len(),
            snapshots: state.snapshots.len(),
            events: state.events.len(),
            cursor: state.cursor,
            poll_secs: self.settings.poll_secs,
            jitter_secs: self.settings.jitter_secs,
            plc_directory: self.settings.plc_directory.clone(),
        }
    }

    pub fn settings(&self) -> &ContinuitySettings {
        &self.settings
    }

    /// Record the highest stream/export cursor seen. Operations for unwatched
    /// DIDs still advance it, so re-adding a DID does not replay the whole log.
    pub async fn set_cursor(&self, seq: u64) {
        let mut state = self.inner.write().await;
        if state.cursor.is_none_or(|current| seq > current) {
            state.cursor = Some(seq);
        }
    }

    pub async fn cursor(&self) -> Option<u64> {
        self.inner.read().await.cursor
    }

    /// Drop a cursor the directory rejected as too old or too new. The next
    /// stream connection starts at the directory's current head.
    pub async fn reset_cursor(&self) {
        self.inner.write().await.cursor = None;
    }

    /// Apply a PLC export operation. Returns the emitted event, if any.
    pub async fn apply_export(
        &self,
        operation: &ExportOperation,
        now: u64,
    ) -> Option<IdentityEvent> {
        self.set_cursor(operation.seq).await;
        let mut state = self.inner.write().await;
        let uuid = state.watched.get(&operation.did).cloned().flatten();
        if !state.watched.contains_key(&operation.did) {
            return None;
        }
        let mut snapshot = snapshot_from_operation(
            &operation.did,
            &operation.operation,
            Some(operation.seq),
            Some(operation.cid.clone()),
            Some(operation.created_at.clone()),
            uuid,
            now,
        );
        // PLC only claims handles. Keep the last verified result for handles
        // the new operation still claims so a fresh event does not drop it;
        // the poll remains the authority for adding new verified handles.
        if let Some(previous) = state.snapshots.get(&operation.did) {
            snapshot.verified_handles = previous
                .verified_handles
                .iter()
                .filter(|handle| snapshot.handles.contains(handle))
                .cloned()
                .collect();
        }
        Self::record(
            &mut state,
            snapshot,
            Some(&operation.created_at),
            self.max_events,
        )
    }

    /// Apply a re-resolution result. `verified_handles` has already passed
    /// bidirectional handle resolution in the watcher.
    pub async fn apply_snapshot(
        &self,
        snapshot: IdentitySnapshot,
        now: u64,
    ) -> Option<IdentityEvent> {
        let mut state = self.inner.write().await;
        if !state.watched.contains_key(&snapshot.did) {
            return None;
        }
        let mut snapshot = snapshot;
        snapshot.observed_at = now;
        if snapshot.uuid.is_none() {
            snapshot.uuid = state.watched.get(&snapshot.did).cloned().flatten();
        }
        Self::record(&mut state, snapshot, None, self.max_events)
    }

    /// Refresh only the verification result without emitting a new event.
    pub async fn verify_handles(&self, did: &str, verified: Vec<String>) {
        let mut state = self.inner.write().await;
        if let Some(snapshot) = state.snapshots.get_mut(did) {
            snapshot.verified_handles = verified;
        }
    }

    fn record(
        state: &mut State,
        snapshot: IdentitySnapshot,
        created_at: Option<&str>,
        max_events: usize,
    ) -> Option<IdentityEvent> {
        let changes = diff_snapshots(state.snapshots.get(&snapshot.did), &snapshot);
        if changes.is_empty() {
            state
                .snapshots
                .entry(snapshot.did.clone())
                .and_modify(|current| *current = snapshot.clone())
                .or_insert(snapshot);
            return None;
        }
        let event = IdentityEvent {
            id: state.next_event_id,
            did: snapshot.did.clone(),
            uuid: snapshot.uuid.clone(),
            changes: changes.clone(),
            seq: snapshot.seq,
            cid: snapshot.cid.clone(),
            created_at: created_at.map(str::to_owned),
            observed_at: snapshot.observed_at,
            previous: state.snapshots.get(&snapshot.did).cloned(),
            current: snapshot.clone(),
        };
        state.next_event_id = state.next_event_id.wrapping_add(1);
        state.snapshots.insert(snapshot.did.clone(), snapshot);
        state.events.push_back(event.clone());
        while state.events.len() > max_events {
            state.events.pop_front();
        }
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::model::IdentityChange;
    use serde_json::json;

    fn operation(kind: &str, endpoint: &str, seq: u64) -> ExportOperation {
        ExportOperation {
            seq,
            did: "did:plc:abcdefghijklmnopqrstuvwx".into(),
            operation: json!({
                "type": kind,
                "rotationKeys": ["did:key:zRotation"],
                "alsoKnownAs": ["at://alice.test"],
                "verificationMethods": {"atproto": "did:key:zSigning"},
                "services": {"atproto_pds": {
                    "type": "AtprotoPersonalDataServer", "endpoint": endpoint
                }}
            }),
            cid: format!("bafy{seq}"),
            created_at: "2026-11-25T00:00:00.000Z".into(),
        }
    }

    #[tokio::test]
    async fn watched_dids_emit_events_and_bound_the_log() {
        let continuity = Continuity::new(ContinuitySettings::default(), 2);
        continuity
            .watch("did:plc:abcdefghijklmnopqrstuvwx", Some("uuid-1".into()))
            .await
            .unwrap();
        let first = continuity
            .apply_export(&operation("plc_operation", "https://a.test", 1), 10)
            .await
            .unwrap();
        assert_eq!(first.changes, [IdentityChange::Observed]);
        assert_eq!(first.uuid.as_deref(), Some("uuid-1"));

        let second = continuity
            .apply_export(&operation("plc_operation", "https://b.test", 2), 20)
            .await
            .unwrap();
        assert_eq!(second.changes, [IdentityChange::PdsMigration]);
        assert_eq!(
            second.previous.unwrap().pds.as_deref(),
            Some("https://a.test")
        );

        let third = continuity
            .apply_export(&operation("plc_operation", "https://c.test", 3), 30)
            .await
            .unwrap();
        assert_eq!(third.id, 2);
        assert_eq!(continuity.events(10).await.len(), 2);
        assert_eq!(continuity.cursor().await, Some(3));
        assert_eq!(continuity.status().await.watched, 1);
    }

    #[tokio::test]
    async fn unwatched_dids_only_advance_the_cursor() {
        let continuity = Continuity::with_settings(ContinuitySettings::default());
        assert!(
            continuity
                .apply_export(&operation("plc_operation", "https://a.test", 5), 1)
                .await
                .is_none()
        );
        assert_eq!(continuity.cursor().await, Some(5));
        assert!(continuity.snapshots().await.is_empty());
    }

    #[tokio::test]
    async fn handles_can_be_verified_without_a_new_event() {
        let continuity = Continuity::with_settings(ContinuitySettings::default());
        continuity
            .watch("did:plc:abcdefghijklmnopqrstuvwx", None)
            .await
            .unwrap();
        continuity
            .apply_export(&operation("plc_operation", "https://a.test", 1), 1)
            .await
            .unwrap();
        continuity
            .verify_handles(
                "did:plc:abcdefghijklmnopqrstuvwx",
                vec!["alice.test".into()],
            )
            .await;
        let snapshot = continuity
            .snapshot("did:plc:abcdefghijklmnopqrstuvwx")
            .await
            .unwrap();
        assert_eq!(snapshot.verified_handles, ["alice.test"]);
        assert_eq!(continuity.events(10).await.len(), 1);

        // A new PLC operation still claims the same handle. Keep the verified
        // result in the event until the poll has a chance to re-check it.
        let next = continuity
            .apply_export(&operation("plc_operation", "https://b.test", 2), 2)
            .await
            .unwrap();
        assert_eq!(next.current.verified_handles, ["alice.test"]);
        assert_eq!(continuity.events(10).await.len(), 2);
    }

    #[tokio::test]
    async fn a_poll_refresh_carries_its_handle_verification() {
        let continuity = Continuity::with_settings(ContinuitySettings::default());
        continuity
            .watch("did:plc:abcdefghijklmnopqrstuvwx", None)
            .await
            .unwrap();
        continuity
            .apply_export(&operation("plc_operation", "https://a.test", 1), 1)
            .await
            .unwrap();
        let mut snapshot = continuity
            .snapshot("did:plc:abcdefghijklmnopqrstuvwx")
            .await
            .unwrap();
        snapshot.verified_handles = vec!["alice.test".into()];
        let event = continuity.apply_snapshot(snapshot, 2).await;
        assert!(event.is_none());
        assert_eq!(
            continuity
                .snapshot("did:plc:abcdefghijklmnopqrstuvwx")
                .await
                .unwrap()
                .verified_handles,
            ["alice.test"]
        );
    }

    #[tokio::test]
    async fn watch_rejects_a_handle() {
        let continuity = Continuity::with_settings(ContinuitySettings::default());
        assert!(continuity.watch("alice.test", None).await.is_err());
        assert!(continuity.watched().await.is_empty());
    }
}
