//! Background PLC stream consumer and jittered re-resolution loop (#190).
//!
//! Both loops are best-effort. The WebSocket gives live PLC operations; the
//! poll re-resolves watched DIDs and bidirectionally verifies every claimed
//! handle. Neither loop touches game state or the join gate.

use super::{
    Continuity, ContinuitySettings,
    feed::{self, FeedMessage},
    model::snapshot_from_did_document,
};
use atmospheric::{
    oauth::{Resolver, resolve_did_document, resolve_handle},
    plc::{PlcOperation, fetch_plc_data},
};
use rand_core::{OsRng, RngCore};
use reqwest::Client;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Owns the two network loops for one sidecar process.
#[derive(Clone)]
pub struct Watcher {
    continuity: Continuity,
    settings: ContinuitySettings,
    resolver: Resolver,
    http: Client,
}

impl Watcher {
    pub fn new(continuity: Continuity, settings: ContinuitySettings, resolver: Resolver) -> Self {
        Self {
            continuity,
            settings,
            resolver,
            http: Client::new(),
        }
    }

    /// Tests and operators with unusual TLS needs can inject an HTTP client.
    pub fn with_http(
        continuity: Continuity,
        settings: ContinuitySettings,
        resolver: Resolver,
        http: Client,
    ) -> Self {
        Self {
            continuity,
            settings,
            resolver,
            http,
        }
    }

    /// Run until the process exits. A failed loop never takes down the other
    /// one: continuity degrades to polling if PLC's stream is unavailable.
    pub async fn run(self) -> Result<(), String> {
        let feed = tokio::spawn(self.clone().feed_loop());
        let poll = tokio::spawn(self.poll_loop());
        let (feed, poll) = tokio::join!(feed, poll);
        feed.map_err(|error| format!("PLC feed task: {error}"))??;
        poll.map_err(|error| format!("PLC poll task: {error}"))??;
        Ok(())
    }

    async fn feed_loop(self) -> Result<(), String> {
        loop {
            let cursor = self.continuity.cursor().await;
            match feed::connect(&self.settings.plc_directory, cursor).await {
                Ok(mut stream) => loop {
                    match feed::next(&mut stream).await {
                        Ok(FeedMessage::Operation(operation)) => {
                            self.continuity.apply_export(&operation, unix_now()).await;
                        }
                        Ok(FeedMessage::Closed(reason)) => {
                            let reason = reason.unwrap_or_else(|| "no reason".into());
                            eprintln!("atmospheric-server: PLC stream closed: {reason}");
                            if reason == "OutdatedCursor" {
                                if let Err(error) = self.catch_up().await {
                                    eprintln!("atmospheric-server: PLC catch-up failed: {error}");
                                }
                            } else if reason == "FutureCursor" {
                                self.continuity.reset_cursor().await;
                            }
                            break;
                        }
                        Ok(FeedMessage::Ended) => {
                            eprintln!("atmospheric-server: PLC stream ended; reconnecting");
                            break;
                        }
                        Err(error) => {
                            eprintln!("atmospheric-server: PLC stream error: {error}");
                            break;
                        }
                    }
                },
                Err(error) => eprintln!("atmospheric-server: {error}; retrying PLC stream"),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Page through sequence-ordered export records after a stale cursor.
    async fn catch_up(&self) -> Result<(), String> {
        let mut after = self.continuity.cursor().await.unwrap_or(0);
        for _ in 0..100 {
            let page = feed::export_page(&self.http, &self.settings.plc_directory, after).await?;
            if page.is_empty() {
                return Ok(());
            }
            for operation in &page {
                after = operation.seq;
                self.continuity.apply_export(operation, unix_now()).await;
            }
            if page.len() < usize::from(super::model::EXPORT_PAGE_SIZE) {
                return Ok(());
            }
        }
        Err("PLC export catch-up exceeded 100 pages".into())
    }

    async fn poll_loop(self) -> Result<(), String> {
        loop {
            for watched in self.continuity.watched().await {
                let watcher = self.clone();
                let delay = jitter(self.settings.jitter_secs);
                tokio::spawn(async move {
                    if delay > 0 {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                    }
                    if let Err(error) = watcher.refresh(&watched.did).await {
                        eprintln!(
                            "atmospheric-server: continuity refresh for {} failed: {error}",
                            watched.did
                        );
                    }
                });
            }
            tokio::time::sleep(Duration::from_secs(self.settings.poll_secs)).await;
        }
    }

    /// Re-resolve one watched DID, fill rotation keys from PLC data, and keep
    /// only handles that resolve back to the same DID.
    async fn refresh(&self, did: &str) -> Result<(), String> {
        let resolver = Resolver {
            handle_service: self.resolver.handle_service.clone(),
            plc_directory: self.settings.plc_directory.clone(),
        };
        let uuid = self
            .continuity
            .watched()
            .await
            .into_iter()
            .find(|watched| watched.did == did)
            .and_then(|watched| watched.uuid);
        let document = resolve_did_document(&self.http, &resolver, did)
            .await
            .map_err(|error| error.to_string())?;
        let mut snapshot = snapshot_from_did_document(did, &document, uuid, unix_now());

        if did.starts_with("did:plc:") {
            let data = fetch_plc_data(&self.http, &self.settings.plc_directory, did)
                .await
                .map_err(|error| error.to_string())?;
            let operation =
                PlcOperation::from_plc_data(&data).map_err(|error| error.to_string())?;
            snapshot.rotation_keys = operation.rotation_keys;
            snapshot.cid = data["cid"].as_str().map(str::to_owned);
        }

        let mut verified = Vec::new();
        for handle in &snapshot.handles {
            match resolve_handle(&self.http, &resolver, handle).await {
                Ok(resolved) if resolved == did => verified.push(handle.clone()),
                Ok(resolved) => eprintln!(
                    "atmospheric-server: {} claims {handle} but it resolves to {resolved}",
                    did
                ),
                Err(error) => {
                    eprintln!("atmospheric-server: handle resolution for {handle} failed: {error}")
                }
            }
        }

        // Verification belongs on the event's current snapshot, not only in a
        // later silent mutation; otherwise consumers see a claim they cannot
        // trust and never learn it was confirmed.
        snapshot.verified_handles = verified;
        self.continuity.apply_snapshot(snapshot, unix_now()).await;
        Ok(())
    }
}

fn jitter(max_secs: u64) -> u64 {
    if max_secs == 0 {
        return 0;
    }
    OsRng.next_u64() % (max_secs + 1)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
