//! Background worker thread: the only place network and disk I/O happens.
//!
//! The game side never awaits anything. It pushes [`Command`]s through one
//! channel and drains [`Event`]s from another once per tick; this thread owns
//! the tokio runtime, the AT Protocol agent, and the settings file under the
//! mod's own storage folder. Keeping that boundary is what makes the rest of
//! the crate testable without a game.

use crate::{
    Command, Config, Event, actions,
    error::AtmosphericError,
    notifications::{self, SeenCursor},
    oauth::{self, DpopState, SharedDpop},
    presence::{self, PlayerDigest},
    records::{self, SnapshotRecord},
    session::{self, Agent, SessionState, StoredSession},
    signing::{self, SigningKey},
    skins,
    tid::TidGen,
};
use ed25519_dalek::VerifyingKey;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    time::Duration,
};

/// The settings file's name inside the mod's storage folder.
pub const CONFIG_FILE: &str = "config.json";
/// The notification cursor's file, same folder.
pub const CURSOR_FILE: &str = "cursor.json";
/// The Ed25519 endorsement key for snapshot records, same folder (M6).
pub const SIGNING_KEY_FILE: &str = "signing-key.json";
/// The snapshot version counter, same folder (M6).
pub const SNAPSHOT_STATE_FILE: &str = "snapshot-state.json";
/// The persisted refresh token, same folder (session resume across restarts).
pub const SESSION_FILE: &str = "session.json";
/// Where verified skin/cape blobs downloaded by M14 reads are saved.
pub const SKINS_DIR: &str = "skins";

/// Write a file that holds secrets (app password in `config.json`, the
/// session/refresh token and DPoP private key in `session.json`, the Ed25519
/// seed in `signing-key.json`) with owner-only permissions: 0600 on unix.
/// Other platforms have no portable mode API, so there the file is written
/// as-is (noted in the README's network-use section).
fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) -> crate::error::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// What the worker needs besides the channels: where its files live.
#[derive(Debug, Clone)]
pub struct WorkerOptions {
    /// The mod's own storage folder (Horizon: `native_data_dir("atmospheric")`,
    /// i.e. `horizon-mods/.horizon-mod-data/atmospheric/`). Never a world save.
    pub storage_dir: PathBuf,
    /// Where handles and DIDs resolve for OAuth logins. The public services
    /// by default; tests point this at a stub.
    pub resolver: oauth::Resolver,
    /// Whether OAuth logins may spawn the system browser. Tests turn this
    /// off so `cargo test` never opens windows on the dev machine.
    pub launch_browser: bool,
}

impl WorkerOptions {
    pub fn new(storage_dir: impl Into<PathBuf>) -> Self {
        Self {
            storage_dir: storage_dir.into(),
            resolver: oauth::Resolver::default(),
            launch_browser: true,
        }
    }

    /// Do not spawn a browser for OAuth logins (tests, headless runs).
    pub fn no_browser(mut self) -> Self {
        self.launch_browser = false;
        self
    }
}

/// Spawn the worker thread.
///
/// `commands` is what the game sends us, `events` is what we report back, and
/// `options` says where the settings file lives. Dropping the command channel
/// (or sending [`Command::Shutdown`]) stops the thread cleanly.
pub fn spawn(
    commands: Receiver<Command>,
    events: Sender<Event>,
    options: WorkerOptions,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("atmospheric-worker".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                let _ = events.send(Event::Failed {
                    message: "could not start the worker runtime".into(),
                });
                return;
            };
            runtime.block_on(async move {
                // Bridge the blocking game-side channel into async land, so
                // the loop below can later select on a notification timer
                // (M4) next to the commands. When the game side drops its
                // sender, this thread ends, `rx` yields `None`, and the loop
                // exits.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
                // Results of spawned OAuth logins (M8) come back here, so the
                // loop stays responsive while the browser is open.
                let (oauth_tx, mut oauth_rx) = tokio::sync::mpsc::unbounded_channel::<
                    crate::error::Result<oauth::OAuthLogin>,
                >();
                let bridge = std::thread::Builder::new()
                    .name("atmospheric-commands".into())
                    .spawn(move || {
                        while let Ok(command) = commands.recv() {
                            if tx.send(command).is_err() {
                                break;
                            }
                        }
                    });
                if let Err(error) = bridge {
                    let _ = events.send(Event::Failed {
                        message: format!("could not start the command bridge: {error}"),
                    });
                    return;
                }

                let mut worker = Worker::new(options, events, oauth_tx);
                worker.emit(Event::Session(SessionState::LoggedOut));
                // Resume a persisted session (refresh token) if there is one.
                worker.try_resume().await;
                // The notification poll timer (M4): ticks at the configured
                // interval once logged in; before that the ticks are no-ops.
                let mut poll =
                    tokio::time::interval(Duration::from_secs(worker.poll_interval_secs));
                poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        command = rx.recv() => {
                            match command {
                                Some(Command::Shutdown) | None => break,
                                Some(command) => {
                                    let login = matches!(command, Command::Login);
                                    worker.handle(command).await;
                                    if login && worker.is_logged_in() {
                                        poll = tokio::time::interval(Duration::from_secs(
                                            worker.poll_interval_secs,
                                        ));
                                        poll.set_missed_tick_behavior(
                                            tokio::time::MissedTickBehavior::Delay,
                                        );
                                    }
                                }
                            }
                        }
                        result = oauth_rx.recv() => {
                            match result {
                                Some(Ok(login)) => {
                                    worker.finish_login_oauth(login).await;
                                    if worker.is_logged_in() {
                                        poll = tokio::time::interval(Duration::from_secs(
                                            worker.poll_interval_secs,
                                        ));
                                        poll.set_missed_tick_behavior(
                                            tokio::time::MissedTickBehavior::Delay,
                                        );
                                    }
                                }
                                Some(Err(error)) => worker.emit(Event::Session(
                                    SessionState::Failed {
                                        message: error.to_string(),
                                    },
                                )),
                                None => {}
                            }
                        }
                        _ = poll.tick() => {
                            worker.poll_notifications(false).await;
                        }
                    }
                }
            });
        })
        .expect("spawn the atmospheric worker thread")
}

/// The persisted snapshot counter (see [`SNAPSHOT_STATE_FILE`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SnapshotState {
    version: i64,
}

/// The worker's state: the agent and DID once logged in, plus its files.
struct Worker {
    storage_dir: PathBuf,
    agent: Option<Agent>,
    did: Option<String>,
    cursor: SeenCursor,
    poll_interval_secs: u64,
    /// TID generator for record keys (chat records, later more).
    tid: TidGen,
    /// `post_chat_messages` from the settings file, refreshed at login.
    post_chat_messages: bool,
    /// `post_death_messages` from the settings file, refreshed at login.
    post_death_messages: bool,
    /// `persist_session` from the settings file, refreshed at login.
    persist_session: bool,
    /// The persisted session, if any, for resuming across restarts.
    stored_session: Option<StoredSession>,
    /// `chat_server_id` from the settings file, refreshed at login.
    chat_server_id: String,
    /// The login identifier, kept so snapshots can default the username.
    identifier: String,
    /// The endorsement signing key, loaded/generated on first snapshot (M6).
    signing_key: Option<SigningKey>,
    /// The last published snapshot version (anti-rollback), persisted.
    snapshot_version: i64,
    /// The shared DPoP state (OAuth mode): installed into every agent built
    /// here, and updated when the access token is refreshed.
    dpop: SharedDpop,
    /// The HTTP client for the raw OAuth calls (resolution, PAR, token).
    http: reqwest::Client,
    /// Where handles/DIDs resolve for OAuth logins.
    resolver: oauth::Resolver,
    /// Whether OAuth logins may spawn the system browser.
    launch_browser: bool,
    /// Where spawned OAuth logins report their result.
    oauth_tx: tokio::sync::mpsc::UnboundedSender<crate::error::Result<oauth::OAuthLogin>>,
    events: Sender<Event>,
}

impl Worker {
    fn new(
        options: WorkerOptions,
        events: Sender<Event>,
        oauth_tx: tokio::sync::mpsc::UnboundedSender<crate::error::Result<oauth::OAuthLogin>>,
    ) -> Self {
        // The seen-cursor persists across restarts, so old notifications are
        // not re-shown; a missing file just means "everything is new".
        let cursor = std::fs::read_to_string(options.storage_dir.join(CURSOR_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let snapshot_version =
            std::fs::read_to_string(options.storage_dir.join(SNAPSHOT_STATE_FILE))
                .ok()
                .and_then(|raw| serde_json::from_str::<SnapshotState>(&raw).ok())
                .map(|state| state.version)
                .unwrap_or(0);
        let stored_session = std::fs::read_to_string(options.storage_dir.join(SESSION_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok());
        Self {
            storage_dir: options.storage_dir,
            agent: None,
            did: None,
            cursor,
            poll_interval_secs: Config::default().poll_interval_secs,
            tid: TidGen::new(),
            post_chat_messages: false,
            post_death_messages: false,
            persist_session: true,
            stored_session,
            chat_server_id: Config::default().chat_server_id,
            identifier: String::new(),
            signing_key: None,
            snapshot_version,
            dpop: oauth::new_shared_dpop(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            resolver: options.resolver.clone(),
            launch_browser: options.launch_browser,
            oauth_tx,
            events,
        }
    }

    fn is_logged_in(&self) -> bool {
        self.agent.is_some() && self.did.is_some()
    }

    fn config_path(&self) -> PathBuf {
        self.storage_dir.join(CONFIG_FILE)
    }

    /// Read the settings file. Only this module touches disk.
    fn load_config(&self) -> crate::error::Result<Config> {
        let raw = std::fs::read_to_string(self.config_path())?;
        Config::from_json(&raw)
    }

    /// Write the settings file, creating the mod's storage folder if needed.
    /// It holds the app password, so it is written owner-only (0600 on unix).
    fn save_config(&self, config: &Config) -> crate::error::Result<()> {
        std::fs::create_dir_all(&self.storage_dir)?;
        write_private(&self.config_path(), config.to_json()?)?;
        Ok(())
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn fail(&self, message: impl Into<String>) {
        self.emit(Event::Failed {
            message: message.into(),
        });
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Shutdown => {} // the loop breaks on this
            Command::SaveConfig { config } => {
                if let Err(error) = self.save_config(&config) {
                    self.fail(format!("could not save the settings file: {error}"));
                }
            }
            Command::Login => self.login().await,
            Command::LoginOAuth { handle } => self.start_login_oauth(handle),
            Command::PostText { text } => {
                self.ensure_oauth_fresh().await;
                let result = match (&self.agent, &self.did) {
                    (Some(agent), Some(did)) => actions::post_text(agent, did, &text).await,
                    _ => Err(AtmosphericError::NotLoggedIn),
                };
                match result {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::PostImage {
                path,
                alt_text,
                text,
            } => {
                self.ensure_oauth_fresh().await;
                match self.post_image(&path, &alt_text, text.as_deref()).await {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::RefreshNotifications => {
                self.ensure_oauth_fresh().await;
                self.poll_notifications(true).await;
            }
            Command::PostSnapshot { mut summary } => {
                self.ensure_oauth_fresh().await;
                let result = self.post_snapshot(&mut summary).await;
                match result {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::PostSkin {
                skin_path,
                cape_path,
                model,
            } => {
                self.ensure_oauth_fresh().await;
                let result = self
                    .post_skin(skin_path.as_deref(), cape_path.as_deref(), &model)
                    .await;
                match result {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::PostServerProfile => {
                self.ensure_oauth_fresh().await;
                match self.post_server_profile().await {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchList { reference } => {
                // Public AppView read: works logged out, no token refresh.
                match self.fetch_list(&reference).await {
                    Ok((list, members)) => self.emit(Event::ListMembers { list, members }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchLabels { subject } => {
                // Public AppView read: works logged out, no token refresh.
                match self.fetch_labels(&subject).await {
                    Ok((subject, via, labels)) => self.emit(Event::Labels {
                        subject,
                        via,
                        labels,
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchLabelerLabels { labeler, subject } => {
                // Public labeler read: works logged out when a subject is
                // given; the default subject (own DID) needs a session.
                match self
                    .fetch_labeler_labels(&labeler, subject.as_deref())
                    .await
                {
                    Ok((subject, via, labels)) => self.emit(Event::Labels {
                        subject,
                        via,
                        labels,
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchPlayer { subject } => {
                // A public read against the subject's own PDS: works logged
                // out, no token refresh.
                match self.fetch_player(&subject).await {
                    Ok((subject, via, snapshot)) => self.emit(Event::PlayerSnapshot {
                        subject,
                        via,
                        snapshot: Box::new(snapshot),
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchPlayerSkin { subject } => {
                // A public read against the subject's own PDS: works logged
                // out, no token refresh.
                match self.fetch_player_skin(&subject).await {
                    Ok((subject, via, skin)) => self.emit(Event::PlayerSkin {
                        subject,
                        via,
                        skin: Box::new(skin),
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::PostDeathMessage { text } => {
                if !self.post_death_messages {
                    return;
                }
                self.ensure_oauth_fresh().await;
                let result = match (&self.agent, &self.did) {
                    (Some(agent), Some(did)) => {
                        actions::post_text(agent, did, &format!("☠ {text}")).await
                    }
                    _ => Err(AtmosphericError::NotLoggedIn),
                };
                match result {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::Logout => {
                self.agent = None;
                self.did = None;
                *self.dpop.lock().unwrap() = None;
                self.stored_session = None;
                let _ = std::fs::remove_file(self.storage_dir.join(SESSION_FILE));
                self.emit(Event::Session(SessionState::LoggedOut));
            }
            Command::PostChat { message, automatic } => {
                // Hook-originated messages only publish when the settings say
                // so; the explicit `/atmospheric chat` command always does.
                if automatic && !self.post_chat_messages {
                    return;
                }
                self.ensure_oauth_fresh().await;
                let result = match (&self.agent, &self.did) {
                    (Some(agent), Some(did)) => {
                        actions::post_chat(
                            agent,
                            did,
                            &self.chat_server_id,
                            &message,
                            &mut self.tid,
                        )
                        .await
                    }
                    _ => Err(AtmosphericError::NotLoggedIn),
                };
                match result {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
        }
    }

    /// Poll notifications (M4): on the timer (`explicit == false`) or on
    /// request. The cursor persists to disk so restarts do not re-show old
    /// notifications. Timer ticks while logged out are silent no-ops.
    async fn poll_notifications(&mut self, explicit: bool) {
        if !self.is_logged_in() {
            if explicit {
                self.fail(AtmosphericError::NotLoggedIn.to_string());
            }
            return;
        }
        let agent = self.agent.as_ref().expect("checked above");
        match notifications::poll(agent, &mut self.cursor).await {
            Ok(items) => {
                if !items.is_empty()
                    && let Err(error) = self.save_cursor()
                {
                    self.fail(format!("could not save the cursor file: {error}"));
                }
                self.emit(Event::Notifications { items });
            }
            Err(error) => {
                if explicit {
                    self.fail(error.to_string());
                }
            }
        }
    }

    /// Persist the seen-cursor (worker owns disk I/O).
    fn save_cursor(&self) -> crate::error::Result<()> {
        let raw = serde_json::to_string(&self.cursor)?;
        std::fs::write(self.storage_dir.join(CURSOR_FILE), raw)?;
        Ok(())
    }

    /// Post an image file: read it here (the worker owns disk I/O), check it
    /// is an image of a size the PDS accepts, then upload + post it.
    async fn post_image(
        &self,
        path: &str,
        alt_text: &str,
        text: Option<&str>,
    ) -> crate::error::Result<String> {
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let bytes = std::fs::read(path).map_err(|error| {
            AtmosphericError::Message(format!("could not read {path}: {error}"))
        })?;
        if bytes.len() > actions::MAX_IMAGE_BYTES {
            return Err(AtmosphericError::Message(format!(
                "{path} is {} bytes; the limit is {}",
                bytes.len(),
                actions::MAX_IMAGE_BYTES
            )));
        }
        // Sniffed to reject non-images early; the PDS re-detects on upload.
        if actions::image_mime(&bytes).is_none() {
            return Err(AtmosphericError::Message(format!(
                "{path} is not a PNG, JPEG, GIF or WebP image"
            )));
        }
        actions::post_image(agent, did, bytes, alt_text, text).await
    }

    /// Publish a player snapshot (M6): load or generate the endorsement key,
    /// build the record at the next version, sign the canonical payload, and
    /// write it under the key id as rkey. The version persists so a restart
    /// never rolls back.
    async fn post_snapshot(
        &mut self,
        summary: &mut records::SnapshotSummary,
    ) -> crate::error::Result<String> {
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        if summary.username.is_empty() {
            summary.username = self.identifier.clone();
        }
        let key = match &self.signing_key {
            Some(key) => key.clone(),
            None => {
                let key = match self.load_or_create_signing_key() {
                    Ok(key) => key,
                    Err(error) => return Err(error),
                };
                self.signing_key = Some(key.clone());
                key
            }
        };
        let key_id = signing::key_id_of(&key.verifying_key());
        let mut record = SnapshotRecord {
            version: self.snapshot_version + 1,
            issued_at: atrium_api::types::string::Datetime::now(),
            summary: summary.clone(),
            aspects: vec![],
            endorsements: vec![],
        };
        let payload = record.signable_payload(did, &key_id);
        record.endorsements.push(records::Endorsement {
            server: key_id.clone(),
            alg: signing::ALG_ED25519.into(),
            sig: records::BytesB64 {
                value: signing::sign_b64(&key, &payload),
            },
        });
        let uri = actions::post_snapshot(agent, did, &record, &key_id).await?;
        self.snapshot_version = record.version;
        if let Err(error) = self.save_snapshot_state() {
            self.fail(format!("could not persist the snapshot version: {error}"));
        }
        Ok(uri)
    }

    /// Publish the player's skin/cape as an `at.atpcraft.player.skin` record
    /// (M9, atpcraft port): upload the PNGs as blobs, then upsert the record
    /// at the endorsing key id (`putRecord`) with a fresh Ed25519
    /// endorsement, keeping endorsements from other keys. `None` paths fall
    /// back to `skin_path`/`cape_path` in the settings file.
    async fn post_skin(
        &mut self,
        skin_path: Option<&str>,
        cape_path: Option<&str>,
        model: &str,
    ) -> crate::error::Result<String> {
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let config = self.load_config().ok();
        let configured = |value: Option<&str>, fallback: &str| {
            value
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .or_else(|| (!fallback.is_empty()).then(|| fallback.to_owned()))
        };
        let skin_path = configured(
            skin_path,
            config.as_ref().map(|c| c.skin_path.as_str()).unwrap_or(""),
        );
        let cape_path = configured(
            cape_path,
            config.as_ref().map(|c| c.cape_path.as_str()).unwrap_or(""),
        );
        let model = records::SkinModel::from_key(model)
            .or_else(|| {
                config
                    .as_ref()
                    .and_then(|c| records::SkinModel::from_key(&c.skin_model))
            })
            .unwrap_or(records::SkinModel::Default);
        if skin_path.is_none() && cape_path.is_none() {
            return Err(AtmosphericError::Message(
                "no skin file: pass a path to /atmospheric skin or set skin_path in config.json"
                    .into(),
            ));
        }
        // The skin lexicon accepts image/png blobs of at most 1 MiB.
        let read_png = |path: &str| -> crate::error::Result<Vec<u8>> {
            let bytes = std::fs::read(path)?;
            if actions::image_mime(&bytes) != Some("image/png") {
                return Err(AtmosphericError::Message(format!(
                    "{path} is not a PNG (the skin lexicon accepts image/png only)"
                )));
            }
            if bytes.len() > records::MAX_SKIN_BYTES {
                return Err(AtmosphericError::Message(format!(
                    "{path} is {} bytes; the skin lexicon caps blobs at {} bytes",
                    bytes.len(),
                    records::MAX_SKIN_BYTES
                )));
            }
            Ok(bytes)
        };
        let skin = match &skin_path {
            Some(path) => Some(actions::upload_blob(agent, read_png(path)?).await?),
            None => None,
        };
        let cape = match &cape_path {
            Some(path) => Some(actions::upload_blob(agent, read_png(path)?).await?),
            None => None,
        };

        let key = match &self.signing_key {
            Some(key) => key.clone(),
            None => {
                let key = self.load_or_create_signing_key()?;
                self.signing_key = Some(key.clone());
                key
            }
        };
        let key_id = signing::key_id_of(&key.verifying_key());

        // Keep endorsements from other keys, like atpcraft's create(existing).
        let mut record = records::SkinRecord {
            skin,
            cape,
            model,
            timestamp: atrium_api::types::string::Datetime::now(),
            endorsements: Vec::new(),
        };
        if let Some(existing) =
            actions::get_record(agent, did, records::SKIN_COLLECTION, &key_id).await?
            && let Ok(existing) =
                serde_json::from_value::<records::SkinRecord>(serde_json::to_value(&existing)?)
        {
            record.endorsements = existing
                .endorsements
                .into_iter()
                .filter(|e| e.server != key_id)
                .collect();
        }
        let payload = record.signable_payload(did, &key_id);
        record.endorsements.push(records::Endorsement {
            server: key_id.clone(),
            alg: signing::ALG_ED25519.into(),
            sig: records::BytesB64 {
                value: signing::sign_b64(&key, &payload),
            },
        });

        actions::put_record(
            agent,
            did,
            records::SKIN_COLLECTION,
            &key_id,
            record.to_unknown()?,
        )
        .await
    }

    /// Publish/refresh the account's server profile
    /// (`at.atpcraft.server.profile` at rkey `self`, M10, atpcraft port).
    /// Identity fields and capabilities come from the settings file; the
    /// `signingKey` block is the mod's persisted Ed25519 key; `createdAt` is
    /// preserved from the existing record.
    async fn post_server_profile(&mut self) -> crate::error::Result<String> {
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let config = self.load_config().unwrap_or_default();
        let key = match &self.signing_key {
            Some(key) => key.clone(),
            None => {
                let key = self.load_or_create_signing_key()?;
                self.signing_key = Some(key.clone());
                key
            }
        };
        let key_id = signing::key_id_of(&key.verifying_key());
        // Preserve createdAt across re-publishes (atpcraft's writer does the
        // same via getRecord).
        let created_at = actions::get_record(
            agent,
            did,
            records::SERVER_PROFILE_COLLECTION,
            records::SERVER_PROFILE_RKEY,
        )
        .await?
        .and_then(|value| serde_json::to_value(&value).ok())
        .and_then(|value| serde_json::from_value::<records::ServerProfileRecord>(value).ok())
        .and_then(|existing| existing.created_at);
        let name = if config.server_name.is_empty() {
            self.identifier.clone()
        } else {
            config.server_name.clone()
        };
        // First publish stamps createdAt too (atpcraft: existing ?: now).
        let now = atrium_api::types::string::Datetime::now();
        let now_str = serde_json::to_value(&now)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let created_at = created_at.or(Some(now_str));
        let record = records::ServerProfileRecord::new(
            name,
            config.server_description.clone(),
            config.server_address.clone(),
            config.game_version.clone(),
            format!("atmospheric-horizon {}", env!("CARGO_PKG_VERSION")),
            records::ProfileSigningKey {
                key_id,
                public_key: signing::public_key_spki_b64(&key.verifying_key()),
                alg: signing::ALG_ED25519.into(),
            },
            config.server_capabilities.clone(),
            Some(records::ProfileOperator {
                did: did.clone(),
                handle: self.identifier.clone(),
                website: config.server_website.clone(),
                contact: config.server_contact.clone(),
            }),
            created_at,
            now,
        );
        actions::put_record(
            agent,
            did,
            records::SERVER_PROFILE_COLLECTION,
            records::SERVER_PROFILE_RKEY,
            record.to_unknown()?,
        )
        .await
    }

    /// Fetch a public Bluesky list's member DIDs (M11, the join-gate read
    /// side of atpcraft's port). Works logged out: `getList` on the public
    /// AppView needs no auth. bsky.app URLs with a handle resolve the handle
    /// first (via the same directory service as OAuth login).
    async fn fetch_list(&self, reference: &str) -> crate::error::Result<(String, Vec<String>)> {
        let config = self.load_config().unwrap_or_default();
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        let list = match crate::lists::parse_list_reference(reference)? {
            crate::lists::ListRef::AtUri(uri) => uri,
            crate::lists::ListRef::NeedsResolve { handle, rkey } => {
                let did = crate::oauth::resolve_handle(&self.http, &self.resolver, &handle).await?;
                format!("at://{did}/app.bsky.graph.list/{rkey}")
            }
        };
        let members = crate::lists::fetch_list_members(&self.http, &appview, &list).await?;
        Ok((list, members))
    }

    /// Read the labels the AppView attaches to a subject's view (M12, the
    /// labeler read side (a)). Works logged out: the public AppView serves the
    /// view endpoints. Handles resolve via the same directory service as
    /// OAuth login.
    async fn fetch_labels(
        &self,
        subject: &str,
    ) -> crate::error::Result<(String, String, Vec<crate::labels::LabelDigest>)> {
        let config = self.load_config().unwrap_or_default();
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        let reference = crate::labels::parse_subject_reference(subject)?;
        let resolved =
            crate::labels::resolve_subject(&self.http, &self.resolver, &reference).await?;
        let labels = crate::labels::view_labels(&self.http, &appview, &resolved).await?;
        Ok((resolved.at_uri, appview, labels))
    }

    /// Query a labeler directly (M12, the labeler read side (b)): resolve the
    /// labeler DID's document, take its `#atproto_labeler` service endpoint,
    /// and page `com.atproto.label.queryLabels` for the subject (default: the
    /// logged-in account's own DID — the atpcraft case is a server labeler
    /// labeling player accounts).
    async fn fetch_labeler_labels(
        &self,
        labeler: &str,
        subject: Option<&str>,
    ) -> crate::error::Result<(String, String, Vec<crate::labels::LabelDigest>)> {
        // The default subject is the logged-in account's DID — fail before any
        // network call when there is no session.
        let pattern = match subject {
            Some(raw) => {
                let reference = crate::labels::parse_subject_reference(raw)?;
                crate::labels::resolve_subject(&self.http, &self.resolver, &reference)
                    .await?
                    .pattern
            }
            None => self.did.clone().ok_or(AtmosphericError::NotLoggedIn)?,
        };
        let labeler_did = if labeler.starts_with("did:") {
            labeler.to_string()
        } else {
            crate::oauth::resolve_handle(&self.http, &self.resolver, labeler).await?
        };
        let doc =
            crate::oauth::resolve_did_document(&self.http, &self.resolver, &labeler_did).await?;
        let endpoint = crate::labels::labeler_endpoint(&doc)?;
        let labels =
            crate::labels::query_labels(&self.http, &endpoint, std::slice::from_ref(&pattern))
                .await?;
        Ok((pattern, format!("{labeler_did} ({endpoint})"), labels))
    }

    /// Read a player's latest `at.atpcraft.player.snapshot` record from their
    /// own PDS and verify its endorsement (M13, the atpcraft presence read
    /// side). Unauthenticated: the subject's PDS serves `listRecords` for
    /// public repos, so this works logged out.
    async fn fetch_player(
        &self,
        subject: &str,
    ) -> crate::error::Result<(String, String, Option<PlayerDigest>)> {
        let config = self.load_config().unwrap_or_default();
        let (did, pds) = presence::resolve_player(&self.http, &self.resolver, subject).await?;
        let records =
            presence::list_records(&self.http, &pds, &did, records::SNAPSHOT_COLLECTION).await?;
        let keys = self.verification_keys(&config);
        let snapshot = presence::latest(&records)
            .map(|(rkey, value)| presence::digest(&did, &rkey, &value, &keys))
            .transpose()?;
        Ok((did, pds, snapshot))
    }

    /// Read a player's latest `at.atpcraft.player.skin` record from their own
    /// PDS, verify the endorsement, and — only when it verifies — download
    /// the skin/cape blobs into the storage folder (M14, the atpcraft
    /// skin-sync read side). The listRecords/getBlob calls are
    /// unauthenticated, so this works logged out.
    async fn fetch_player_skin(
        &self,
        subject: &str,
    ) -> crate::error::Result<(String, String, Option<skins::SkinDigest>)> {
        let config = self.load_config().unwrap_or_default();
        let (did, pds) = presence::resolve_player(&self.http, &self.resolver, subject).await?;
        let records =
            presence::list_records(&self.http, &pds, &did, records::SKIN_COLLECTION).await?;
        let keys = self.verification_keys(&config);
        let Some((rkey, value)) = skins::latest_skin(&records) else {
            return Ok((did, pds, None));
        };
        let mut digest = skins::digest(&did, &rkey, &value, &keys)?;
        // Only verified records have their blobs persisted — an unverified
        // record is reported (with its verdict) but nothing is downloaded.
        if matches!(digest.verdict, records::EndorsementVerdict::Verified { .. }) {
            for blob in [&mut digest.skin, &mut digest.cape].into_iter().flatten() {
                self.save_verified_blob(&pds, &did, blob).await?;
            }
        }
        Ok((did, pds, Some(digest)))
    }

    /// Download one blob and save it under `<storage>/skins/<cid>.png`,
    /// rejecting non-PNG content and anything over the lexicon's 1 MiB cap
    /// before writing (the worker owns disk I/O).
    async fn save_verified_blob(
        &self,
        pds: &str,
        did: &str,
        blob: &mut skins::BlobDigest,
    ) -> crate::error::Result<()> {
        if blob.cid.is_empty() {
            return Ok(());
        }
        let (mime, bytes) = skins::get_blob(&self.http, pds, did, &blob.cid).await?;
        if crate::actions::image_mime(&bytes) != Some("image/png") {
            return Err(AtmosphericError::Message(format!(
                "blob {} is not a PNG (content-type {mime})",
                blob.cid
            )));
        }
        if bytes.len() > records::MAX_SKIN_BYTES {
            return Err(AtmosphericError::Message(format!(
                "blob {} is over the lexicon's 1 MiB cap ({} bytes)",
                blob.cid,
                bytes.len()
            )));
        }
        blob.mime_type = mime;
        blob.size = bytes.len() as u64;
        let dir = self.storage_dir.join(SKINS_DIR);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.png", blob.cid));
        std::fs::write(&path, &bytes)?;
        blob.path = Some(path.display().to_string());
        Ok(())
    }

    /// The endorsement verification keys for presence reads (M13): the mod's
    /// own signing key when one is persisted (not created just for a read),
    /// plus the `trusted_server_keys` from the settings file. Malformed
    /// entries are skipped, as atpcraft's `TrustedServerKeys` does — a skipped
    /// key simply shows up as an unknown endorser in the verdict.
    fn verification_keys(&self, config: &Config) -> HashMap<String, VerifyingKey> {
        let mut keys = HashMap::new();
        if let Ok(raw) = std::fs::read_to_string(self.storage_dir.join(SIGNING_KEY_FILE))
            && let Ok(file) = serde_json::from_str::<signing::SigningKeyFile>(&raw)
            && let Ok(key) = signing::from_key_file(&file)
        {
            keys.insert(file.key_id, key.verifying_key());
        }
        for entry in &config.trusted_server_keys {
            if let Ok(key) = signing::verifying_key_from_spki_b64(&entry.public_key) {
                let key_id = if entry.key_id.is_empty() {
                    signing::key_id_of(&key)
                } else {
                    entry.key_id.clone()
                };
                keys.insert(key_id, key);
            }
        }
        keys
    }

    /// Load the endorsement key from the storage folder, or generate and
    /// persist a fresh one (worker owns disk I/O).
    fn load_or_create_signing_key(&self) -> crate::error::Result<SigningKey> {
        let path = self.storage_dir.join(SIGNING_KEY_FILE);
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let file: signing::SigningKeyFile = serde_json::from_str(&raw)?;
            return signing::from_key_file(&file);
        }
        let key = signing::generate();
        let file = signing::key_file_of(&key);
        std::fs::create_dir_all(&self.storage_dir)?;
        write_private(&path, serde_json::to_string_pretty(&file)?)?;
        Ok(key)
    }

    fn save_snapshot_state(&self) -> crate::error::Result<()> {
        let state = SnapshotState {
            version: self.snapshot_version,
        };
        std::fs::write(
            self.storage_dir.join(SNAPSHOT_STATE_FILE),
            serde_json::to_string(&state)?,
        )?;
        Ok(())
    }

    /// App-password login: read the settings file, log in, cache the agent
    /// and the DID from `get_session`.
    async fn login(&mut self) {
        self.emit(Event::Session(SessionState::LoggingIn));
        let config = match self.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.emit(Event::Session(SessionState::Failed {
                    message: format!("could not read the settings file: {error}"),
                }));
                return;
            }
        };
        if !config.is_usable() {
            self.emit(Event::Session(SessionState::Failed {
                message: format!(
                    "not configured: set pds_url, identifier and app_password in {}",
                    self.config_path().display()
                ),
            }));
            return;
        }
        match session::login(&config, self.dpop.clone()).await {
            Ok((agent, stored)) => {
                // Session check after login, as the docs promise.
                if let Err(error) = agent.api.com.atproto.server.get_session().await {
                    self.emit(Event::Session(SessionState::Failed {
                        message: AtmosphericError::protocol(error).to_string(),
                    }));
                    return;
                }
                let did = stored.did.clone();
                self.agent = Some(agent);
                self.did = Some(did.clone());
                self.poll_interval_secs = config.poll_interval_secs.max(5);
                self.post_chat_messages = config.post_chat_messages;
                self.post_death_messages = config.post_death_messages;
                self.persist_session = config.persist_session;
                self.chat_server_id = config.chat_server_id.clone();
                self.identifier = config.identifier.clone();
                if config.persist_session {
                    self.stored_session = Some(stored.clone());
                    if let Err(error) = self.save_session(&stored) {
                        self.fail(format!("could not persist the session file: {error}"));
                    }
                }
                self.emit(Event::Session(SessionState::LoggedIn { did }));
            }
            Err(error) => self.emit(Event::Session(SessionState::Failed {
                message: AtmosphericError::protocol(error).to_string(),
            })),
        }
    }

    /// Start a localhost OAuth login (M8). The browser flow runs on a spawned
    /// task so the command loop stays responsive (Shutdown works while the
    /// browser is open); its result arrives on the oauth channel.
    fn start_login_oauth(&mut self, handle: String) {
        self.emit(Event::Session(SessionState::LoggingIn));
        // The client_id comes from the settings file when set (hosted client
        // metadata URL); otherwise the loopback default.
        let client_id = self
            .load_config()
            .map(|config| config.oauth_client_id(oauth::DEFAULT_SCOPE))
            .unwrap_or_else(|_| oauth::client_id(oauth::DEFAULT_SCOPE));
        let http = self.http.clone();
        let events = self.events.clone();
        let oauth_tx = self.oauth_tx.clone();
        let resolver = self.resolver.clone();
        let launch_browser = self.launch_browser;
        tokio::spawn(async move {
            let result = oauth::run_login(oauth::LoginFlow {
                http: &http,
                resolver: &resolver,
                handle: &handle,
                client_id: &client_id,
                scope: oauth::DEFAULT_SCOPE,
                redirect_timeout: oauth::REDIRECT_WAIT,
                launch_browser,
                events: &events,
            })
            .await;
            let _ = oauth_tx.send(result);
        });
    }

    /// Apply a finished OAuth login: build the DPoP-signed agent, validate the
    /// session, persist the refresh token and DPoP key, report LoggedIn.
    async fn finish_login_oauth(&mut self, login: oauth::OAuthLogin) {
        let oauth::OAuthLogin {
            tokens,
            key,
            pds_url,
            did,
            handle,
            token_endpoint,
            issuer,
            client_id,
            scope,
        } = login;
        match session::agent_for_oauth(&pds_url, &did, &handle, &tokens, key.clone()).await {
            Ok((agent, dpop)) => {
                self.agent = Some(agent);
                self.did = Some(did.clone());
                self.dpop = dpop;
                self.identifier = handle.clone();
                // The settings file may carry newer flags/intervals.
                if let Ok(config) = self.load_config() {
                    self.poll_interval_secs = config.poll_interval_secs.max(5);
                    self.post_chat_messages = config.post_chat_messages;
                    self.post_death_messages = config.post_death_messages;
                    self.persist_session = config.persist_session;
                    self.chat_server_id = config.chat_server_id.clone();
                }
                let stored = StoredSession {
                    pds_url,
                    did: did.clone(),
                    handle: handle.clone(),
                    refresh_jwt: tokens.refresh_token.clone().unwrap_or_default(),
                    oauth: Some(session::StoredOAuth {
                        dpop_jwk: key.to_jwk_json(),
                        token_endpoint,
                        issuer,
                        client_id,
                        scope,
                    }),
                };
                self.stored_session = Some(stored.clone());
                if self.persist_session
                    && let Err(error) = self.save_session(&stored)
                {
                    self.fail(format!("could not persist the session file: {error}"));
                }
                self.emit(Event::Session(SessionState::LoggedIn { did }));
            }
            Err(error) => self.emit(Event::Session(SessionState::Failed {
                message: error.to_string(),
            })),
        }
    }

    /// Refresh the OAuth access token when it is close to expiry. The agent's
    /// own ExpiredToken auto-refresh only knows the app-password
    /// `refreshSession` endpoint, which an OAuth session cannot use, so the
    /// worker refreshes proactively at the authorization server instead.
    async fn ensure_oauth_fresh(&mut self) {
        let needs_refresh = self
            .dpop
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|state| state.expires_within(60));
        if !needs_refresh {
            return;
        }
        let (Some(state), Some(mut stored)) = (
            self.dpop.lock().unwrap().clone(),
            self.stored_session.clone(),
        ) else {
            return;
        };
        let Some(oauth) = stored.oauth.clone() else {
            return;
        };
        match oauth::refresh(
            &self.http,
            &oauth.token_endpoint,
            &oauth.client_id,
            &state.refresh_token,
            &state.key,
        )
        .await
        {
            Ok(tokens) => {
                *self.dpop.lock().unwrap() = Some(DpopState::from_tokens(state.key, &tokens));
                stored.refresh_jwt = tokens.refresh_token.clone().unwrap_or(stored.refresh_jwt);
                self.stored_session = Some(stored.clone());
                if self.persist_session
                    && let Err(error) = self.save_session(&stored)
                {
                    self.fail(format!("could not persist the session file: {error}"));
                }
            }
            Err(error) => self.fail(format!("could not refresh the OAuth session: {error}")),
        }
    }

    /// Resume a persisted session (refresh token) without the app password.
    /// Runs once at startup; failure just leaves the worker logged out.
    async fn try_resume(&mut self) {
        let Some(stored) = self.stored_session.clone() else {
            return;
        };
        // An OAuth session resumes through the authorization server's token
        // endpoint (DPoP-bound refresh), not the PDS's refreshSession.
        if let Some(oauth) = &stored.oauth {
            match session::resume_oauth(&self.http, &stored, oauth).await {
                Ok((agent, dpop, rotated)) => {
                    let did = rotated.did.clone();
                    self.agent = Some(agent);
                    self.did = Some(did.clone());
                    self.dpop = dpop;
                    self.identifier = rotated.handle.clone();
                    self.stored_session = Some(rotated.clone());
                    if let Err(error) = self.save_session(&rotated) {
                        self.fail(format!("could not persist the session file: {error}"));
                    }
                    if let Ok(config) = self.load_config() {
                        self.poll_interval_secs = config.poll_interval_secs.max(5);
                        self.post_chat_messages = config.post_chat_messages;
                        self.post_death_messages = config.post_death_messages;
                        self.persist_session = config.persist_session;
                        self.chat_server_id = config.chat_server_id.clone();
                    }
                    self.emit(Event::Session(SessionState::LoggedIn { did }));
                }
                Err(error) => {
                    // Expired/revoked OAuth refresh token (or the
                    // authorization server unreachable): stay logged out,
                    // but say so instead of failing silently (#18).
                    self.stored_session = None;
                    self.emit(Event::Session(SessionState::Failed {
                        message: format!(
                            "could not resume the stored OAuth session ({error}) — log in again with /atmospheric login <handle>"
                        ),
                    }));
                }
            }
            return;
        }
        match session::resume(&stored, self.dpop.clone()).await {
            Ok((agent, stored)) => {
                let did = stored.did.clone();
                self.agent = Some(agent);
                self.did = Some(did.clone());
                self.stored_session = Some(stored.clone());
                if let Err(error) = self.save_session(&stored) {
                    self.fail(format!("could not persist the session file: {error}"));
                }
                // The settings file may carry newer flags/intervals.
                if let Ok(config) = self.load_config() {
                    self.poll_interval_secs = config.poll_interval_secs.max(5);
                    self.post_chat_messages = config.post_chat_messages;
                    self.post_death_messages = config.post_death_messages;
                    self.persist_session = config.persist_session;
                    self.chat_server_id = config.chat_server_id.clone();
                    self.identifier = config.identifier.clone();
                }
                self.emit(Event::Session(SessionState::LoggedIn { did }));
            }
            Err(error) => {
                // Expired/revoked refresh token (or the PDS unreachable):
                // stay logged out, but say so instead of failing silently
                // (#18). The persisted file stays, so a later restart with
                // network access retries the resume.
                self.stored_session = None;
                self.emit(Event::Session(SessionState::Failed {
                    message: format!(
                        "could not resume the stored session ({error}) — log in again with /atmospheric login"
                    ),
                }));
            }
        }
    }

    /// Persist the session's refresh token (worker owns disk I/O). For OAuth
    /// sessions the file also holds the DPoP private key, so it is written
    /// owner-only (0600 on unix).
    fn save_session(&self, stored: &StoredSession) -> crate::error::Result<()> {
        std::fs::create_dir_all(&self.storage_dir)?;
        write_private(
            &self.storage_dir.join(SESSION_FILE),
            serde_json::to_string_pretty(stored)?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::Duration,
    };

    /// A unique temporary folder, without pulling in a tempdir crate.
    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "atmospheric-worker-test-{}-{}-{tag}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_config(pds_url: String) -> Config {
        Config {
            pds_url,
            identifier: "stub.test".into(),
            app_password: "correct-horse".into(),
            poll_interval_secs: 60,
            post_death_messages: false,
            post_chat_messages: false,
            chat_server_id: "test-server".into(),
            persist_session: true,
            oauth_client_id: String::new(),
            skin_path: String::new(),
            cape_path: String::new(),
            skin_model: "default".into(),
            server_name: String::new(),
            server_description: String::new(),
            server_address: String::new(),
            server_website: String::new(),
            server_contact: String::new(),
            server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
            game_version: "26.3".into(),
            appview_url: "https://public.api.bsky.app".into(),
            trusted_server_keys: Vec::new(),
        }
    }

    fn write_config(dir: &std::path::Path, config: &Config) {
        std::fs::write(dir.join(CONFIG_FILE), config.to_json().unwrap()).unwrap();
    }

    /// One captured HTTP request: the path and the body (raw and as text).
    #[derive(Debug, Clone)]
    struct Captured {
        path: String,
        body: String,
        body_bytes: Vec<u8>,
        authorized: bool,
    }

    /// A stub PDS on loopback: createSession, getSession, createRecord,
    /// putRecord/getRecord (backed by a small in-memory store) and
    /// uploadBlob, with every request recorded for the assertions.
    struct StubPds {
        url: String,
        requests: Arc<Mutex<Vec<Captured>>>,
        records: Arc<Mutex<std::collections::HashMap<String, String>>>,
        shutdown: Arc<Mutex<bool>>,
    }

    impl StubPds {
        /// `accept_bad_password` makes createSession answer 401, to exercise
        /// the login failure path.
        fn start(accept_bad_password: bool) -> Self {
            Self::start_with(accept_bad_password, false)
        }

        /// `reject_seen_at` makes listNotifications answer 400 when a seenAt
        /// parameter is present, like some third-party PDSs do.
        fn start_with(accept_bad_password: bool, reject_seen_at: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let records = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let uploads = Arc::new(Mutex::new(0usize));
            let shutdown = Arc::new(Mutex::new(false));
            {
                let requests = Arc::clone(&requests);
                let records = Arc::clone(&records);
                let uploads = Arc::clone(&uploads);
                let shutdown = Arc::clone(&shutdown);
                let url = url.clone();
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if *shutdown.lock().unwrap() {
                            break;
                        }
                        let Ok(mut stream) = stream else { break };
                        let requests = Arc::clone(&requests);
                        let records = Arc::clone(&records);
                        let uploads = Arc::clone(&uploads);
                        let url = url.clone();
                        std::thread::spawn(move || {
                            handle_connection(
                                &mut stream,
                                &url,
                                &requests,
                                &records,
                                &uploads,
                                accept_bad_password,
                                reject_seen_at,
                            );
                        });
                    }
                });
            }
            Self {
                url,
                requests,
                records,
                shutdown,
            }
        }

        fn requests(&self) -> Vec<Captured> {
            self.requests.lock().unwrap().clone()
        }

        /// Insert a record directly, as if a createRecord/putRecord had
        /// stored it (M13 presence-read tests seed foreign-endorsed records).
        fn seed_record(&self, collection: &str, rkey: &str, value: serde_json::Value) {
            self.records
                .lock()
                .unwrap()
                .insert(format!("{collection}/{rkey}"), value.to_string());
        }

        /// The stored putRecord bodies, keyed by `<collection>/<rkey>`.
        fn stored_records(&self) -> Vec<(String, String)> {
            let mut entries: Vec<_> = self
                .records
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            entries.sort();
            entries
        }
    }

    impl Drop for StubPds {
        fn drop(&mut self) {
            *self.shutdown.lock().unwrap() = true;
        }
    }

    /// Read one HTTP request and answer it like a PDS would. The stub also
    /// doubles as the handle service, PLC directory, AppView and labeler for
    /// the M11/M12 read-side tests (`server_url` goes into the DID docs).
    fn handle_connection(
        stream: &mut std::net::TcpStream,
        server_url: &str,
        requests: &Arc<Mutex<Vec<Captured>>>,
        records: &Arc<Mutex<std::collections::HashMap<String, String>>>,
        uploads: &Arc<Mutex<usize>>,
        accept_bad_password: bool,
        reject_seen_at: bool,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if buf.len() > 1 << 20 {
                return;
            }
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = headers.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let mut content_length = 0usize;
        let mut authorized = false;
        let mut refresh_authorized = false;
        for line in lines {
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
            if lower.starts_with("authorization:") {
                if lower.contains("bearer stub-access") {
                    authorized = true;
                }
                if lower.contains("bearer stub-refresh") {
                    refresh_authorized = true;
                }
            }
        }
        let mut body = buf[header_end + 4..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
            }
        }
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let body_string = String::from_utf8_lossy(&body).to_string();
        requests.lock().unwrap().push(Captured {
            path: path.clone(),
            body: body_string.clone(),
            body_bytes: body.clone(),
            authorized,
        });

        let (status, payload): (&str, String) = if path
            .starts_with("/xrpc/app.bsky.notification.listNotifications")
        {
            // First poll (no seenAt) returns two notifications; once the
            // client sends seenAt, the list is empty.
            if !authorized {
                (
                    "401 Unauthorized",
                    r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                )
            } else if path.contains("seenAt=") && reject_seen_at {
                (
                    "400 Bad Request",
                    r#"{"error":"InvalidRequest","message":"The seenAt parameter is unsupported"}"#
                        .to_string(),
                )
            } else if path.contains("seenAt=") {
                (
                    "200 OK",
                    r#"{"seenAt":"2026-11-12T12:00:00.000Z","notifications":[]}"#.to_string(),
                )
            } else {
                (
                    "200 OK",
                    r#"{"cursor":"c2","seenAt":"2026-11-12T12:00:00.000Z","notifications":[{"uri":"at://did:plc:alice/app.bsky.feed.like/1","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","indexedAt":"2026-11-12T10:00:00.000Z","isRead":false,"reason":"like","record":{"$type":"app.bsky.feed.like"},"author":{"did":"did:plc:alice","handle":"alice.test"}},{"uri":"at://did:plc:bob/app.bsky.feed.post/2","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","indexedAt":"2026-11-12T11:00:00.000Z","isRead":false,"reason":"reply","record":{"$type":"app.bsky.feed.post","text":"nice post!"},"author":{"did":"did:plc:bob","handle":"bob.test"}}]}"#.to_string(),
                )
            }
        } else {
            match path.split('?').next().unwrap_or(&path) {
                "/xrpc/com.atproto.server.createSession" => {
                    if accept_bad_password || !body_string.contains("correct-horse") {
                        (
                        "401 Unauthorized",
                        r#"{"error":"AuthenticationRequired","message":"Invalid identifier or password"}"#.to_string(),
                    )
                    } else {
                        (
                        "200 OK",
                        r#"{"did":"did:plc:stubuser","handle":"stub.test","accessJwt":"stub-access","refreshJwt":"stub-refresh","active":true}"#.to_string(),
                    )
                    }
                }
                "/xrpc/com.atproto.server.refreshSession" => {
                    if refresh_authorized {
                        (
                            "200 OK",
                            r#"{"did":"did:plc:stubuser","handle":"stub.test","accessJwt":"stub-access","refreshJwt":"stub-refresh-2","active":true}"#.to_string(),
                        )
                    } else {
                        (
                            "400 Bad Request",
                            r#"{"error":"ExpiredToken","message":"refresh token expired"}"#
                                .to_string(),
                        )
                    }
                }
                "/xrpc/com.atproto.server.getSession" => {
                    if authorized {
                        (
                            "200 OK",
                            r#"{"did":"did:plc:stubuser","handle":"stub.test","active":true}"#
                                .to_string(),
                        )
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                "/xrpc/com.atproto.repo.uploadBlob" => {
                    if authorized {
                        let size = body.len();
                        // A real CID per upload (alternating), so two uploads
                        // (skin + cape) get distinct, parseable $links.
                        let mut count = uploads.lock().unwrap();
                        let cid = STUB_CIDS[*count % STUB_CIDS.len()];
                        *count += 1;
                        drop(count);
                        let payload = serde_json::json!({
                            "blob": {
                                "$type": "blob",
                                "ref": {"$link": cid},
                                "mimeType": "image/png",
                                "size": size,
                            }
                        });
                        ("200 OK", payload.to_string())
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                "/xrpc/com.atproto.repo.putRecord" => {
                    if authorized {
                        let body_json: serde_json::Value =
                            serde_json::from_str(&body_string).unwrap_or_default();
                        let collection = body_json["collection"].as_str().unwrap_or("");
                        let rkey = body_json["rkey"].as_str().unwrap_or("");
                        records.lock().unwrap().insert(
                            format!("{collection}/{rkey}"),
                            body_json["record"].to_string(),
                        );
                        let payload = serde_json::json!({
                            "uri": format!("at://did:plc:stubuser/{collection}/{rkey}"),
                            "cid": STUB_CIDS[0],
                        });
                        ("200 OK", payload.to_string())
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                "/xrpc/com.atproto.repo.getRecord" => {
                    // getRecord passes repo/collection/rkey as query params.
                    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
                    let param = |name: &str| {
                        query.split('&').find_map(|pair| {
                            let (k, v) = pair.split_once('=')?;
                            (k == name).then(|| v.to_string())
                        })
                    };
                    let key = format!(
                        "{}/{}",
                        param("collection").unwrap_or_default(),
                        param("rkey").unwrap_or_default()
                    );
                    if !authorized {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    } else if let Some(record) = records.lock().unwrap().get(&key).cloned() {
                        (
                            "200 OK",
                            format!(
                                r#"{{"uri":"at://did:plc:stubuser/{key}","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","value":{record}}}"#
                            ),
                        )
                    } else {
                        (
                            "400 Bad Request",
                            r#"{"error":"RecordNotFound","message":"record not found"}"#
                                .to_string(),
                        )
                    }
                }
                "/xrpc/app.bsky.graph.getList" => {
                    // Two pages: cursor "c2" on the first, none on the second.
                    // The first page's list view carries labels (M12).
                    if path.contains("cursor=c2") {
                        (
                            "200 OK",
                            r#"{"items":[{"subject":{"did":"did:plc:carol","handle":"carol.test"}}]}"#
                                .to_string(),
                        )
                    } else if path.contains("list=") {
                        (
                            "200 OK",
                            r#"{"list":{"uri":"at://did:plc:owner/app.bsky.graph.list/3abc","labels":[{"ver":1,"src":"did:plc:labeler","uri":"at://did:plc:owner/app.bsky.graph.list/3abc","val":"curated","cts":"2026-11-12T09:00:00.000Z"}]},"items":[{"subject":{"did":"did:plc:alice","handle":"alice.test"}},{"subject":{"did":"did:plc:bob","handle":"bob.test"}}],"cursor":"c2"}"#.to_string(),
                        )
                    } else {
                        (
                            "400 Bad Request",
                            r#"{"error":"InvalidRequest","message":"missing list"}"#.to_string(),
                        )
                    }
                }
                // The stub doubles as the handle service for M11/M12 reads.
                "/xrpc/com.atproto.identity.resolveHandle" => (
                    "200 OK",
                    r#"{"did":"did:plc:stubuser"}"#.to_string(),
                ),
                // The stub doubles as the PLC directory; the DID documents
                // advertise the stub itself as PDS and (for the labeler DIDs)
                // labeler service endpoint.
                "/did:plc:stubuser" | "/did:plc:labeler" => {
                    let doc = serde_json::json!({
                        "@context": ["https://www.w3.org/ns/did/v1"],
                        "id": "did:plc:stubuser",
                        "service": [
                            {"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": server_url},
                            {"id": "#atproto_labeler", "type": "AtprotoLabeler", "serviceEndpoint": server_url}
                        ],
                    });
                    ("200 OK", doc.to_string())
                }
                "/did:plc:nolabeler" => {
                    let doc = serde_json::json!({
                        "id": "did:plc:nolabeler",
                        "service": [
                            {"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": server_url}
                        ],
                    });
                    ("200 OK", doc.to_string())
                }
                // A DID whose repo the M13 test leaves empty (no records).
                "/did:plc:empty" => {
                    let doc = serde_json::json!({
                        "id": "did:plc:empty",
                        "service": [
                            {"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": server_url}
                        ],
                    });
                    ("200 OK", doc.to_string())
                }
                // The stub doubles as the AppView for the M12 view-label reads.
                "/xrpc/app.bsky.actor.getProfile" => (
                    "200 OK",
                    r#"{"did":"did:plc:stubuser","handle":"stub.test","labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:stubuser","val":"player","cts":"2026-11-12T10:00:00.000Z"}]}"#.to_string(),
                ),
                "/xrpc/app.bsky.feed.getPosts" => (
                    "200 OK",
                    r#"{"posts":[{"uri":"at://did:plc:stubuser/app.bsky.feed.post/3abc","labels":[{"ver":1,"src":"did:plc:labeler","uri":"at://did:plc:stubuser/app.bsky.feed.post/3abc","val":"spam","neg":true,"cts":"2026-11-12T11:00:00.000Z","exp":"2026-12-12T11:00:00.000Z"}]}]}"#.to_string(),
                ),
                // The stub doubles as a labeler for the M12 queryLabels read:
                // two pages, cursor "2" on the first.
                "/xrpc/com.atproto.label.queryLabels" => {
                    if !path.contains("uriPatterns=") {
                        (
                            "400 Bad Request",
                            r#"{"error":"InvalidRequest","message":"Parameter 'uriPatterns' is required"}"#.to_string(),
                        )
                    } else if path.contains("cursor=2") {
                        (
                            "200 OK",
                            r#"{"labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:stubuser","val":"online","cts":"2026-11-12T12:00:00.000Z"}]}"#.to_string(),
                        )
                    } else {
                        (
                            "200 OK",
                            r#"{"labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:stubuser","val":"player","cts":"2026-11-12T10:00:00.000Z"}],"cursor":"2"}"#.to_string(),
                        )
                    }
                }
                "/xrpc/com.atproto.repo.createRecord" => {
                    if authorized {
                        // Store the record like a real PDS, so listRecords
                        // (M13) can serve it back; posts get no rkey, so the
                        // stub picks the fixed one the older tests assert on.
                        let body_json: serde_json::Value =
                            serde_json::from_str(&body_string).unwrap_or_default();
                        let collection = body_json["collection"].as_str().unwrap_or("");
                        let rkey = body_json["rkey"].as_str().unwrap_or("3jub4stub").to_string();
                        records.lock().unwrap().insert(
                            format!("{collection}/{rkey}"),
                            body_json["record"].to_string(),
                        );
                        let payload = serde_json::json!({
                            "uri": format!("at://did:plc:stubuser/{collection}/{rkey}"),
                            "cid": STUB_CIDS[0],
                        });
                        ("200 OK", payload.to_string())
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                // The stub serves listRecords unauthenticated (public repos
                // do), filtering the stored records by collection (M13).
                "/xrpc/com.atproto.repo.listRecords" => {
                    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
                    let param = |name: &str| {
                        query.split('&').find_map(|pair| {
                            let (k, v) = pair.split_once('=')?;
                            (k == name).then(|| v.to_string())
                        })
                    };
                    let collection = param("collection").unwrap_or_default();
                    // Query values arrive percent-encoded; the repo DID's ':'
                    // comes as %3A.
                    let repo = param("repo").unwrap_or_default().replace("%3A", ":");
                    let prefix = format!("{collection}/");
                    // The stub hosts one real repo (did:plc:stubuser); other
                    // repos (e.g. did:plc:empty) have no stored records.
                    let mut items: Vec<serde_json::Value> = if repo == "did:plc:stubuser" {
                        records
                            .lock()
                            .unwrap()
                            .iter()
                            .filter_map(|(key, value)| {
                                key.strip_prefix(&prefix).map(|rkey| {
                                    serde_json::json!({
                                        "uri": format!("at://{repo}/{collection}/{rkey}"),
                                        "cid": STUB_CIDS[0],
                                        "value": serde_json::from_str::<serde_json::Value>(value)
                                            .unwrap_or_default(),
                                    })
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    items.sort_by(|a, b| a["uri"].as_str().cmp(&b["uri"].as_str()));
                    (
                        "200 OK",
                        serde_json::json!({ "records": items }).to_string(),
                    )
                }
                _ => (
                    "404 Not Found",
                    r#"{"error":"UnknownEndpoint"}"#.to_string(),
                ),
            }
        };
        // getBlob answers with raw bytes and the blob's content type, not
        // JSON — the M14 read side downloads blobs through it.
        if path.starts_with("/xrpc/com.atproto.sync.getBlob") {
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
            let cid = query
                .split('&')
                .find_map(|pair| {
                    pair.split_once('=')
                        .filter(|(k, _)| *k == "cid")
                        .map(|(_, v)| v.to_string())
                })
                .unwrap_or_default();
            if STUB_CIDS.contains(&cid.as_str()) {
                let png = tiny_png();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    png.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&png);
            } else {
                let body = r#"{"error":"BlobNotFound","message":"no such blob"}"#;
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
            let _ = stream.flush();
            return;
        }
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Real CIDs the stub hands out for uploads (a fake one would fail to
    /// parse as a CidLink on the client). Alternated so skin + cape differ.
    const STUB_CIDS: [&str; 2] = [
        "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy",
        "bafkreibme22gw2h7y2h7tg2fhqotaqjucnbc24deqo72b6mkl2egezxhvy",
    ];

    fn spawn_worker(
        storage_dir: PathBuf,
    ) -> (
        mpsc::Sender<Command>,
        mpsc::Receiver<Event>,
        std::thread::JoinHandle<()>,
    ) {
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let handle = spawn(
            command_rx,
            event_tx,
            WorkerOptions::new(storage_dir).no_browser(),
        );
        (command_tx, event_rx, handle)
    }

    /// Spawn a worker whose handle/DID resolution points at a stub (M11/M12
    /// read-side tests).
    fn spawn_worker_with_resolver(
        storage_dir: PathBuf,
        resolver: crate::oauth::Resolver,
    ) -> (
        mpsc::Sender<Command>,
        mpsc::Receiver<Event>,
        std::thread::JoinHandle<()>,
    ) {
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let options = WorkerOptions {
            storage_dir,
            resolver,
            launch_browser: false,
        };
        let handle = spawn(command_rx, event_tx, options);
        (command_tx, event_rx, handle)
    }

    /// Wait for an event matching `pred`, collecting the rest.
    fn wait_for(event_rx: &mpsc::Receiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for a worker event");
            match event_rx.recv_timeout(remaining) {
                Ok(event) if pred(&event) => return event,
                Ok(_) => continue,
                Err(error) => panic!("worker event channel failed: {error}"),
            }
        }
    }

    #[test]
    fn worker_shuts_down_on_command_and_on_channel_drop() {
        // Command::Shutdown stops the thread.
        let dir = temp_dir("shutdown");
        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        // Drain the startup event, then shut down.
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedOut))
        });
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        // The thread is gone: no more events arrive.
        assert!(event_rx.recv_timeout(Duration::from_millis(200)).is_err());

        // Dropping the command channel stops the thread too.
        let dir = temp_dir("drop");
        let (command_tx, _event_rx, handle) = spawn_worker(dir);
        drop(command_tx);
        handle.join().unwrap();
    }

    #[test]
    fn login_fails_against_a_rejecting_pds() {
        let pds = StubPds::start(true);
        let dir = temp_dir("login-failure");
        write_config(&dir, &test_config(pds.url.clone()));

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();

        // The worker reports the attempt, then the failure.
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggingIn))
        });
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::Failed { .. }))
        }) {
            Event::Session(SessionState::Failed { message }) => {
                assert!(!message.is_empty());
            }
            other => panic!("expected Session(Failed), got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The PDS saw the login attempt.
        let requests = pds.requests();
        let paths: Vec<_> = requests.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"/xrpc/com.atproto.server.createSession"));
    }

    #[test]
    fn login_and_post_text_against_a_stub_pds() {
        let pds = StubPds::start(false);
        let dir = temp_dir("login-post");
        write_config(&dir, &test_config(pds.url.clone()));

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        }) {
            Event::Session(SessionState::LoggedIn { did }) => {
                assert_eq!(did, "did:plc:stubuser");
            }
            other => panic!("expected Session(LoggedIn), got {other:?}"),
        }

        command_tx
            .send(Command::PostText {
                text: "hello from the worker test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                assert_eq!(uri, "at://did:plc:stubuser/app.bsky.feed.post/3jub4stub");
            }
            other => panic!("expected Posted, got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The PDS saw an authorised session check and a well-formed record.
        let requests = pds.requests();
        let get_session = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.server.getSession")
            .expect("getSession was called");
        assert!(
            get_session.authorized,
            "getSession carried the access token"
        );
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .expect("createRecord was called");
        assert!(create_record.authorized);
        let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
        assert_eq!(body["collection"], "app.bsky.feed.post");
        assert_eq!(body["repo"], "did:plc:stubuser");
        assert_eq!(body["record"]["$type"], "app.bsky.feed.post");
        assert_eq!(body["record"]["text"], "hello from the worker test");
    }

    #[test]
    fn post_without_login_fails_without_touching_the_network() {
        let pds = StubPds::start(false);
        let dir = temp_dir("not-logged-in");
        write_config(&dir, &test_config(pds.url.clone()));

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx
            .send(Command::PostText { text: "hi".into() })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("not logged in")),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        assert!(pds.requests().is_empty());
    }

    #[test]
    fn save_config_persists_the_settings_file() {
        let dir = temp_dir("save-config");
        let (command_tx, _event_rx, handle) = spawn_worker(dir.clone());

        let config = Config {
            pds_url: "https://pds.example".into(),
            identifier: "someone.example".into(),
            app_password: "abcd-1234-efgh-5678".into(),
            poll_interval_secs: 30,
            post_death_messages: true,
            post_chat_messages: true,
            chat_server_id: "my-server".into(),
            persist_session: true,
            oauth_client_id: String::new(),
            skin_path: String::new(),
            cape_path: String::new(),
            skin_model: "default".into(),
            server_name: String::new(),
            server_description: String::new(),
            server_address: String::new(),
            server_website: String::new(),
            server_contact: String::new(),
            server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
            game_version: "26.3".into(),
            appview_url: "https://public.api.bsky.app".into(),
            trusted_server_keys: Vec::new(),
        };
        command_tx
            .send(Command::SaveConfig {
                config: config.clone(),
            })
            .unwrap();
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let raw = std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap();
        let back = Config::from_json(&raw).unwrap();
        assert_eq!(back.pds_url, config.pds_url);
        assert_eq!(back.identifier, config.identifier);
        assert_eq!(back.app_password, config.app_password);
        assert_eq!(back.poll_interval_secs, 30);
        assert!(back.post_death_messages);
        assert!(back.post_chat_messages);
        assert_eq!(back.chat_server_id, "my-server");
        assert!(back.persist_session);
    }

    /// A minimal PNG header (magic + IHDR with 2560x1440) for image tests.
    fn tiny_png() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&2560u32.to_be_bytes());
        png.extend_from_slice(&1440u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, colour type, …
        png
    }

    #[test]
    fn post_image_uploads_the_blob_and_posts_the_embed() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-image");
        write_config(&dir, &test_config(pds.url.clone()));
        let png = tiny_png();
        let shot = dir.join("shot.png");
        std::fs::write(&shot, &png).unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx
            .send(Command::PostImage {
                path: shot.display().to_string(),
                alt_text: "test screenshot".into(),
                text: Some("with a caption".into()),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                assert_eq!(uri, "at://did:plc:stubuser/app.bsky.feed.post/3jub4stub")
            }
            other => panic!("expected Posted, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let requests = pds.requests();
        let upload = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.uploadBlob")
            .expect("uploadBlob was called");
        assert!(upload.authorized);
        assert_eq!(upload.body_bytes, png, "the raw image bytes were uploaded");
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .expect("createRecord was called");
        let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
        assert_eq!(body["record"]["embed"]["$type"], "app.bsky.embed.images");
        assert_eq!(
            body["record"]["embed"]["images"][0]["alt"],
            "test screenshot"
        );
        assert_eq!(
            body["record"]["embed"]["images"][0]["image"]["$type"],
            "blob"
        );
        assert_eq!(
            body["record"]["embed"]["images"][0]["aspectRatio"]["width"],
            2560
        );
        assert_eq!(
            body["record"]["embed"]["images"][0]["aspectRatio"]["height"],
            1440
        );
        assert_eq!(body["record"]["text"], "with a caption");
    }

    #[test]
    fn post_image_rejects_missing_and_non_image_files_without_uploading() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-image-bad");
        write_config(&dir, &test_config(pds.url.clone()));
        let not_image = dir.join("notes.txt");
        std::fs::write(&not_image, b"just text").unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        command_tx
            .send(Command::PostImage {
                path: dir.join("missing.png").display().to_string(),
                alt_text: "x".into(),
                text: None,
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("could not read")),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx
            .send(Command::PostImage {
                path: not_image.display().to_string(),
                alt_text: "x".into(),
                text: None,
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("not a PNG, JPEG, GIF or WebP")),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        assert!(
            !pds.requests()
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.repo.uploadBlob"),
            "nothing was uploaded for the rejected files"
        );
    }

    #[test]
    fn notifications_poll_dedupes_with_the_persisted_cursor() {
        let pds = StubPds::start(false);
        let dir = temp_dir("notifications");
        let mut config = test_config(pds.url.clone());
        config.poll_interval_secs = 3600; // only the first (immediate) tick fires
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        // The timer's first tick polls right after login: two new items.
        match wait_for(
            &event_rx,
            |e| matches!(e, Event::Notifications { items } if items.len() == 2),
        ) {
            Event::Notifications { items } => {
                assert_eq!(items[0].author_handle, "alice.test");
                assert_eq!(items[0].reason, "like");
                assert_eq!(items[1].author_handle, "bob.test");
                assert_eq!(items[1].reason, "reply");
                assert_eq!(items[1].text_snippet, "nice post!");
            }
            other => panic!("expected Notifications(2), got {other:?}"),
        }

        // An explicit refresh now sees nothing new (the cursor advanced).
        command_tx.send(Command::RefreshNotifications).unwrap();
        match wait_for(
            &event_rx,
            |e| matches!(e, Event::Notifications { items } if items.is_empty()),
        ) {
            Event::Notifications { items } => assert!(items.is_empty()),
            other => panic!("expected Notifications(0), got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The cursor was persisted, at the newest notification's timestamp.
        let raw = std::fs::read_to_string(dir.join(CURSOR_FILE)).unwrap();
        let cursor: notifications::SeenCursor = serde_json::from_str(&raw).unwrap();
        assert_eq!(cursor.seen_at.as_deref(), Some("2026-11-12T11:00:00.000Z"));
    }

    #[test]
    fn post_chat_writes_an_atpcraft_chat_record_keyed_by_a_tid() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-chat");
        let mut config = test_config(pds.url.clone());
        config.post_chat_messages = true;
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        command_tx
            .send(Command::PostChat {
                message: "hello from in-game chat".into(),
                automatic: true,
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                // The stub now answers createRecord with the real
                // collection/rkey, so the chat record's URI carries its TID.
                assert!(
                    uri.starts_with("at://did:plc:stubuser/at.atpcraft.player.chat/"),
                    "{uri}"
                );
            }
            other => panic!("expected Posted, got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let requests = pds.requests();
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .expect("createRecord was called");
        assert!(create_record.authorized);
        let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
        assert_eq!(body["collection"], "at.atpcraft.player.chat");
        assert_eq!(body["repo"], "did:plc:stubuser");
        // The rkey is a 13-char sortable TID.
        let rkey = body["rkey"].as_str().expect("rkey was sent");
        assert_eq!(rkey.len(), 13);
        assert!(crate::tid::decode(rkey).is_some());
        // The record carries the atpcraft $type and the configured server id.
        assert_eq!(body["record"]["$type"], "at.atpcraft.player.chat");
        assert_eq!(body["record"]["message"], "hello from in-game chat");
        assert_eq!(body["record"]["server"], "test-server");
        assert!(body["record"]["sentAt"].is_string());
    }

    #[test]
    fn automatic_chat_is_skipped_unless_enabled_but_the_command_always_posts() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-chat-flag");
        let mut config = test_config(pds.url.clone());
        config.post_chat_messages = false; // the default: off
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        // An automatic (chat-hook) message with the flag off: no network call.
        command_tx
            .send(Command::PostChat {
                message: "should not be published".into(),
                automatic: true,
            })
            .unwrap();
        // The explicit command always publishes, even with the flag off.
        command_tx
            .send(Command::PostChat {
                message: "explicitly published".into(),
                automatic: false,
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => assert!(!uri.is_empty()),
            other => panic!("expected Posted, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // Exactly one createRecord went out, and it carried the explicit text.
        let requests = pds.requests();
        let create_records: Vec<_> = requests
            .iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .collect();
        assert_eq!(create_records.len(), 1, "only the explicit post went out");
        let body: serde_json::Value = serde_json::from_str(&create_records[0].body).unwrap();
        assert_eq!(body["record"]["message"], "explicitly published");
    }

    #[test]
    fn notifications_fall_back_when_the_pds_rejects_seen_at() {
        // Some third-party PDSs answer 400 to the seenAt hint; the worker
        // retries without it and the persisted cursor still dedupes.
        let pds = StubPds::start_with(false, true);
        let dir = temp_dir("notifications-seenat-400");
        let mut config = test_config(pds.url.clone());
        config.poll_interval_secs = 3600; // only the first (immediate) tick fires
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        // First poll (no cursor yet): both notifications, via the fallback.
        match wait_for(
            &event_rx,
            |e| matches!(e, Event::Notifications { items } if items.len() == 2),
        ) {
            Event::Notifications { items } => assert_eq!(items.len(), 2),
            other => panic!("expected Notifications(2), got {other:?}"),
        }

        // The explicit refresh sends seenAt, gets the 400, retries without it,
        // and the local cursor hides the already-seen items: no failure event.
        command_tx.send(Command::RefreshNotifications).unwrap();
        match wait_for(
            &event_rx,
            |e| matches!(e, Event::Notifications { items } if items.is_empty()),
        ) {
            Event::Notifications { items } => assert!(items.is_empty()),
            other => panic!("expected Notifications(0), got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The stub did see a seenAt request (which it rejected) and then a
        // retry without it.
        let polls: Vec<_> = pds
            .requests()
            .iter()
            .filter(|r| {
                r.path
                    .starts_with("/xrpc/app.bsky.notification.listNotifications")
            })
            .map(|r| r.path.clone())
            .collect();
        assert!(
            polls.iter().any(|p| p.contains("seenAt=")),
            "the seenAt hint was sent: {polls:?}"
        );
        assert!(
            polls.iter().any(|p| !p.contains("seenAt=")),
            "and retried without it: {polls:?}"
        );
    }

    /// The summary the snapshot test posts.
    fn test_summary() -> records::SnapshotSummary {
        records::SnapshotSummary {
            username: String::new(), // the worker fills it from the identifier
            dimension: "minecraft:overworld".into(),
            position: records::Position {
                x: -25,
                y: 67,
                z: -3,
            },
            health: "20".into(),
            food: 20,
            xp_level: 0,
            game_mode: "survival".into(),
            team: None,
            scores: None,
        }
    }

    #[test]
    fn post_snapshot_writes_a_signed_atpcraft_record_and_advances_the_version() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-snapshot");
        write_config(&dir, &test_config(pds.url.clone()));

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        // Two snapshots: versions 1 and 2, same key id as rkey.
        for expected_version in [1, 2] {
            command_tx
                .send(Command::PostSnapshot {
                    summary: test_summary(),
                })
                .unwrap();
            match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
                Event::Posted { uri } => assert!(!uri.is_empty()),
                other => panic!("expected Posted, got {other:?}"),
            }
            let requests = pds.requests();
            let create_record = requests
                .iter()
                .rev()
                .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
                .expect("createRecord was called");
            let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
            assert_eq!(body["collection"], "at.atpcraft.player.snapshot");
            assert_eq!(body["record"]["$type"], "at.atpcraft.player.snapshot");
            assert_eq!(body["record"]["version"], expected_version);
            // The username defaulted to the configured identifier.
            assert_eq!(body["record"]["summary"]["username"], "stub.test");
            // rkey == endorsements[0].server == the persisted key id.
            let rkey = body["rkey"].as_str().unwrap();
            assert_eq!(rkey.len(), 16);
            let endorsement = &body["record"]["endorsements"][0];
            assert_eq!(endorsement["server"], rkey);
            assert_eq!(endorsement["alg"], "ed25519");
            let sig = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD_NO_PAD,
                endorsement["sig"]["$bytes"].as_str().unwrap(),
            )
            .unwrap();
            assert_eq!(sig.len(), 64, "an Ed25519 signature is 64 bytes");

            // The signature verifies against the persisted public key over the
            // canonical payload rebuilt from the record itself.
            let key_file: signing::SigningKeyFile =
                serde_json::from_str(&std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap())
                    .unwrap();
            assert_eq!(key_file.key_id, rkey);
            let key = signing::from_key_file(&key_file).unwrap();
            let payload = records::SnapshotRecord {
                version: body["record"]["version"].as_i64().unwrap(),
                issued_at: body["record"]["issuedAt"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap(),
                summary: serde_json::from_value(body["record"]["summary"].clone()).unwrap(),
                aspects: vec![],
                endorsements: vec![],
            };
            // A verifier rebuilds the canonical payload from the record and
            // checks the signature against the persisted public key.
            let signable = payload.signable_payload("did:plc:stubuser", rkey);
            signing::verify_b64(
                &key.verifying_key(),
                &signable,
                endorsement["sig"]["$bytes"].as_str().unwrap(),
            )
            .unwrap();
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The version persisted for the next run.
        let state: SnapshotState =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SNAPSHOT_STATE_FILE)).unwrap())
                .unwrap();
        assert_eq!(state.version, 2);
    }

    #[test]
    fn post_skin_uploads_the_blob_and_upserts_an_endorsed_record() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-skin");
        write_config(&dir, &test_config(pds.url.clone()));
        let skin = dir.join("skin.png");
        std::fs::write(&skin, tiny_png()).unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx
            .send(Command::PostSkin {
                skin_path: Some(skin.display().to_string()),
                cape_path: None,
                model: "slim".into(),
            })
            .unwrap();
        let rkey = match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                let rkey = uri.rsplit('/').next().unwrap().to_string();
                assert!(
                    uri.starts_with("at://did:plc:stubuser/at.atpcraft.player.skin/"),
                    "{uri}"
                );
                rkey
            }
            other => panic!("expected Posted, got {other:?}"),
        };
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The rkey is the persisted signing key's id.
        let key_file: signing::SigningKeyFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap())
                .unwrap();
        assert_eq!(rkey, key_file.key_id);

        let requests = pds.requests();
        let upload = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.uploadBlob")
            .expect("the skin PNG was uploaded as a blob");
        assert_eq!(upload.body_bytes, tiny_png());
        let get = requests
            .iter()
            .find(|r| r.path.starts_with("/xrpc/com.atproto.repo.getRecord"))
            .expect("the existing record was fetched to keep foreign endorsements");
        assert!(get.path.contains("collection=at.atpcraft.player.skin"));
        assert!(get.path.contains(&format!("rkey={rkey}")));
        let put = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.putRecord")
            .expect("putRecord upserted the skin record");
        let body: serde_json::Value = serde_json::from_str(&put.body).unwrap();
        assert_eq!(body["collection"], "at.atpcraft.player.skin");
        assert_eq!(body["rkey"], rkey);
        assert_eq!(body["validate"], false);
        let record = &body["record"];
        assert_eq!(record["$type"], "at.atpcraft.player.skin");
        assert_eq!(record["model"], "slim");
        assert_eq!(record["skin"]["$type"], "blob");
        assert_eq!(record["skin"]["mimeType"], "image/png");
        assert!(record.get("cape").is_none(), "no cape was sent");
        assert_eq!(record["endorsements"].as_array().unwrap().len(), 1);
        assert_eq!(record["endorsements"][0]["server"], rkey);
        assert_eq!(record["endorsements"][0]["alg"], "ed25519");

        // The endorsement verifies against the persisted key over the
        // canonical payload (atpcraft's signablePayload shape).
        let key = signing::from_key_file(&key_file).unwrap();
        let stored: records::SkinRecord = serde_json::from_value(record.clone()).unwrap();
        let payload = stored.signable_payload("did:plc:stubuser", &rkey);
        signing::verify_b64(
            &key.verifying_key(),
            &payload,
            record["endorsements"][0]["sig"]["$bytes"].as_str().unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn post_skin_keeps_foreign_endorsements_on_republish() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-skin-foreign");
        write_config(&dir, &test_config(pds.url.clone()));
        let skin = dir.join("skin.png");
        std::fs::write(&skin, tiny_png()).unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx
            .send(Command::PostSkin {
                skin_path: Some(skin.display().to_string()),
                cape_path: None,
                model: "default".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));

        // Another "server" endorses the same record on the PDS.
        let key_file: signing::SigningKeyFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap())
                .unwrap();
        let store_key = format!("at.atpcraft.player.skin/{}", key_file.key_id);
        {
            let mut records = pds.records.lock().unwrap();
            let mut record: serde_json::Value = serde_json::from_str(&records[&store_key]).unwrap();
            record["endorsements"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "server": "foreignserverkey1",
                    "alg": "ed25519",
                    "sig": {"$bytes": "Zm9yZWlnbg"}
                }));
            records.insert(store_key.clone(), record.to_string());
        }

        // Re-publish: the foreign endorsement survives, ours is replaced.
        command_tx
            .send(Command::PostSkin {
                skin_path: Some(skin.display().to_string()),
                cape_path: None,
                model: "default".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let stored = pds
            .stored_records()
            .into_iter()
            .find(|(k, _)| k == &store_key)
            .map(|(_, v)| v)
            .unwrap();
        let record: serde_json::Value = serde_json::from_str(&stored).unwrap();
        let servers: Vec<&str> = record["endorsements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["server"].as_str().unwrap())
            .collect();
        assert!(servers.contains(&"foreignserverkey1"), "{servers:?}");
        assert_eq!(
            servers.iter().filter(|s| **s == key_file.key_id).count(),
            1,
            "the local endorsement was replaced, not duplicated: {servers:?}"
        );
    }

    #[test]
    fn post_skin_rejects_non_png_and_missing_paths_without_uploading() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-skin-bad");
        write_config(&dir, &test_config(pds.url.clone()));
        let not_png = dir.join("notes.txt");
        std::fs::write(&not_png, b"just text").unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        // No path argument and nothing configured: refused before any I/O.
        command_tx
            .send(Command::PostSkin {
                skin_path: None,
                cape_path: None,
                model: "default".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("no skin file"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        // A non-PNG file is refused too.
        command_tx
            .send(Command::PostSkin {
                skin_path: Some(not_png.display().to_string()),
                cape_path: None,
                model: "default".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("not a PNG"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        assert!(
            !pds.requests()
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.repo.uploadBlob"),
            "nothing was uploaded"
        );
    }

    #[test]
    fn post_server_profile_upserts_the_descriptor_at_self() {
        let pds = StubPds::start(false);
        let dir = temp_dir("server-profile");
        let mut config = test_config(pds.url.clone());
        config.server_name = "Test Server".into();
        config.server_address = "play.example.com:25565".into();
        config.server_capabilities =
            vec!["skin-sync".into(), "join-gate".into(), "skin-sync".into()];
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx.send(Command::PostServerProfile).unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                assert_eq!(uri, "at://did:plc:stubuser/at.atpcraft.server.profile/self");
            }
            other => panic!("expected Posted, got {other:?}"),
        }
        // Re-publish: createdAt is preserved, updatedAt moves on.
        command_tx.send(Command::PostServerProfile).unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let key_file: signing::SigningKeyFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap())
                .unwrap();
        let requests = pds.requests();
        assert!(
            requests
                .iter()
                .any(|r| r.path.starts_with("/xrpc/com.atproto.repo.getRecord")
                    && r.path.contains("collection=at.atpcraft.server.profile")
                    && r.path.contains("rkey=self")),
            "the existing profile was fetched to preserve createdAt"
        );
        let puts: Vec<&Captured> = requests
            .iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.putRecord")
            .collect();
        assert_eq!(puts.len(), 2, "two publishes = two upserts");
        let body: serde_json::Value = serde_json::from_str(&puts[1].body).unwrap();
        assert_eq!(body["collection"], "at.atpcraft.server.profile");
        assert_eq!(body["rkey"], "self");
        assert_eq!(body["validate"], false);
        let record = &body["record"];
        assert_eq!(record["$type"], "at.atpcraft.server.profile");
        assert_eq!(record["name"], "Test Server");
        assert_eq!(record["address"], "play.example.com:25565");
        assert_eq!(record["minecraftVersion"], "26.3");
        assert!(
            record["atpcraftVersion"]
                .as_str()
                .unwrap()
                .starts_with("atmospheric-horizon "),
            "{}",
            record["atpcraftVersion"]
        );
        // capabilities sorted + deduped (the lexicon requires it)
        assert_eq!(record["capabilities"][0], "join-gate");
        assert_eq!(record["capabilities"][1], "skin-sync");
        assert_eq!(record["capabilities"].as_array().unwrap().len(), 2);
        // signingKey block matches the persisted key
        assert_eq!(record["signingKey"]["keyId"], key_file.key_id);
        assert_eq!(record["signingKey"]["alg"], "ed25519");
        let key = signing::from_key_file(&key_file).unwrap();
        assert_eq!(
            record["signingKey"]["publicKey"].as_str().unwrap(),
            signing::public_key_spki_b64(&key.verifying_key())
        );
        // operator block
        assert_eq!(record["operator"]["did"], "did:plc:stubuser");
        assert_eq!(record["operator"]["handle"], "stub.test");
        // no per-record signature (repo-authenticated), createdAt preserved
        assert!(record.get("sig").is_none() && record.get("endorsements").is_none());
        let first: serde_json::Value = serde_json::from_str(
            &serde_json::from_str::<serde_json::Value>(&puts[0].body).unwrap()["record"]
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            record["createdAt"].as_str().unwrap(),
            first["createdAt"].as_str().unwrap(),
            "createdAt survives re-publish"
        );
        assert!(!record["updatedAt"].as_str().unwrap().is_empty());
    }

    #[test]
    fn fetch_list_paginates_the_appview_and_reports_members() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-list");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone(); // the stub doubles as the AppView
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx
            .send(Command::FetchList {
                reference: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ListMembers { .. })) {
            Event::ListMembers { list, members } => {
                assert_eq!(list, "at://did:plc:owner/app.bsky.graph.list/3abc");
                assert_eq!(
                    members,
                    vec![
                        "did:plc:alice".to_string(),
                        "did:plc:bob".to_string(),
                        "did:plc:carol".to_string()
                    ]
                );
            }
            other => panic!("expected ListMembers, got {other:?}"),
        }
        // A bad reference fails without any HTTP call.
        command_tx
            .send(Command::FetchList {
                reference: "https://example.com/nope".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("not a list reference"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let hits = pds
            .requests()
            .into_iter()
            .filter(|r| r.path.starts_with("/xrpc/app.bsky.graph.getList"))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 2, "two pages fetched: {hits:?}");
        assert!(hits[0].path.contains("limit=100"));
        assert!(hits[1].path.contains("cursor=c2"));
    }

    #[test]
    fn fetch_labels_reads_view_labels_from_the_appview() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-labels");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone(); // the stub doubles as the AppView
        write_config(&dir, &config);
        let resolver = crate::oauth::Resolver {
            handle_service: pds.url.clone(),
            plc_directory: pds.url.clone(),
        };
        let (command_tx, event_rx, handle) = spawn_worker_with_resolver(dir.clone(), resolver);

        // A post subject: the getPosts view carries the labels.
        command_tx
            .send(Command::FetchLabels {
                subject: "at://did:plc:stubuser/app.bsky.feed.post/3abc".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Labels { .. })) {
            Event::Labels {
                subject,
                via,
                labels,
            } => {
                assert_eq!(subject, "at://did:plc:stubuser/app.bsky.feed.post/3abc");
                assert_eq!(via, pds.url);
                assert_eq!(labels.len(), 1);
                assert_eq!(labels[0].val, "spam");
                assert!(labels[0].neg);
                assert_eq!(labels[0].src, "did:plc:labeler");
            }
            other => panic!("expected Labels, got {other:?}"),
        }

        // A handle account subject: resolveHandle first, then getProfile.
        command_tx
            .send(Command::FetchLabels {
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Labels { .. })) {
            Event::Labels {
                subject, labels, ..
            } => {
                assert_eq!(subject, "at://did:plc:stubuser");
                assert_eq!(labels.len(), 1);
                assert_eq!(labels[0].val, "player");
            }
            other => panic!("expected Labels, got {other:?}"),
        }

        // A list subject reads the labels off the list view (limit=1).
        command_tx
            .send(Command::FetchLabels {
                subject: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Labels { .. })) {
            Event::Labels { labels, .. } => {
                assert_eq!(labels.len(), 1);
                assert_eq!(labels[0].val, "curated");
            }
            other => panic!("expected Labels, got {other:?}"),
        }

        // A bad subject fails without any HTTP call.
        let before = pds.requests().len();
        command_tx
            .send(Command::FetchLabels {
                subject: "https://example.com/nope".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("not a subject reference"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            pds.requests().len(),
            before,
            "no HTTP call for a bad subject"
        );

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let hits = pds
            .requests()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>();
        assert!(
            hits.iter()
                .any(|p| p.starts_with("/xrpc/app.bsky.feed.getPosts?uris=")),
            "{hits:?}"
        );
        assert!(
            hits.iter()
                .any(|p| p == "/xrpc/com.atproto.identity.resolveHandle?handle=stub.test"),
            "{hits:?}"
        );
        assert!(
            hits.iter()
                .any(|p| p == "/xrpc/app.bsky.actor.getProfile?actor=did:plc:stubuser"),
            "{hits:?}"
        );
        assert!(
            hits.iter()
                .any(|p| p.starts_with("/xrpc/app.bsky.graph.getList?list=")
                    && p.contains("limit=1")),
            "{hits:?}"
        );
    }

    #[test]
    fn fetch_labeler_labels_resolves_the_did_doc_and_queries_the_labeler() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-labeler");
        write_config(&dir, &test_config(pds.url.clone()));
        let resolver = crate::oauth::Resolver {
            handle_service: pds.url.clone(),
            plc_directory: pds.url.clone(),
        };
        let (command_tx, event_rx, handle) = spawn_worker_with_resolver(dir.clone(), resolver);

        // A labeler DID with an explicit subject: DID doc → #atproto_labeler
        // endpoint → paged queryLabels.
        command_tx
            .send(Command::FetchLabelerLabels {
                labeler: "did:plc:labeler".into(),
                subject: Some("did:plc:stubuser".into()),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Labels { .. })) {
            Event::Labels {
                subject,
                via,
                labels,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert!(via.contains("did:plc:labeler"), "{via}");
                assert!(via.contains(&pds.url), "{via}");
                assert_eq!(labels.len(), 2);
                assert_eq!(labels[0].val, "player");
                assert_eq!(labels[1].val, "online");
            }
            other => panic!("expected Labels, got {other:?}"),
        }

        // A DID without a labeler service fails after the DID-doc read.
        command_tx
            .send(Command::FetchLabelerLabels {
                labeler: "did:plc:nolabeler".into(),
                subject: Some("did:plc:stubuser".into()),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("#atproto_labeler"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        // Logged out with no subject: fails as not logged in, before any
        // labeler HTTP call.
        let before = pds.requests().len();
        command_tx
            .send(Command::FetchLabelerLabels {
                labeler: "did:plc:labeler".into(),
                subject: None,
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("not logged in"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(pds.requests().len(), before, "no HTTP call when logged out");

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let hits = pds
            .requests()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>();
        assert!(hits.iter().any(|p| p == "/did:plc:labeler"), "{hits:?}");
        assert!(hits.iter().any(|p| p == "/did:plc:nolabeler"), "{hits:?}");
        let queries: Vec<&String> = hits
            .iter()
            .filter(|p| p.starts_with("/xrpc/com.atproto.label.queryLabels"))
            .collect();
        assert_eq!(queries.len(), 2, "two pages queried: {queries:?}");
        assert!(queries[0].contains("uriPatterns=did%3Aplc%3Astubuser"));
        assert!(queries[0].contains("limit=100"));
        assert!(queries[1].contains("cursor=2"));
    }

    /// A resolver pointing handle resolution and the PLC directory at the
    /// stub (the stub doubles as both, M11/M12/M13 read-side tests).
    fn stub_resolver(pds_url: &str) -> crate::oauth::Resolver {
        crate::oauth::Resolver {
            handle_service: pds_url.to_string(),
            plc_directory: pds_url.to_string(),
        }
    }

    #[test]
    fn fetch_player_reads_and_verifies_the_latest_snapshot() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-player");
        write_config(&dir, &test_config(pds.url.clone()));

        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        // Publish a snapshot: the stub stores the createRecord body, so the
        // read below finds it.
        command_tx
            .send(Command::PostSnapshot {
                summary: test_summary(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));

        // Read it back by handle (the stub doubles as the handle service).
        command_tx
            .send(Command::FetchPlayer {
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSnapshot { .. })) {
            Event::PlayerSnapshot {
                subject,
                via,
                snapshot,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                let d = snapshot.expect("a snapshot record exists");
                assert_eq!(d.username, "stub.test", "filled from the identifier");
                assert_eq!(d.version, 1);
                assert_eq!(d.dimension, "minecraft:overworld");
                assert_eq!((d.x, d.y, d.z), (-25, 67, -3));
                // Verified by the mod's own persisted signing key.
                let key_file: signing::SigningKeyFile = serde_json::from_str(
                    &std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap(),
                )
                .unwrap();
                assert_eq!(d.rkey, key_file.key_id);
                assert_eq!(
                    d.verdict,
                    records::EndorsementVerdict::Verified {
                        endorser: key_file.key_id.clone()
                    }
                );
                assert_eq!(
                    d.at_uri,
                    format!(
                        "at://did:plc:stubuser/{}/{}",
                        records::SNAPSHOT_COLLECTION,
                        key_file.key_id
                    )
                );
            }
            other => panic!("expected PlayerSnapshot, got {other:?}"),
        }
        // The listRecords call went out unauthenticated against the
        // subject's own PDS.
        let list_hit = pds
            .requests()
            .into_iter()
            .find(|r| r.path.starts_with("/xrpc/com.atproto.repo.listRecords"))
            .expect("listRecords was called");
        assert!(!list_hit.authorized, "the read is unauthenticated");
        assert!(list_hit.path.contains("repo=did%3Aplc%3Astubuser"));
        assert!(
            list_hit
                .path
                .contains("collection=at.atpcraft.player.snapshot")
        );
        assert!(list_hit.path.contains("limit=100"));
        let hits: Vec<String> = pds.requests().into_iter().map(|r| r.path).collect();
        assert!(
            hits.iter()
                .any(|p| p.starts_with("/xrpc/com.atproto.identity.resolveHandle?handle=stub.test")),
            "{hits:?}"
        );
        assert!(hits.iter().any(|p| p == "/did:plc:stubuser"), "{hits:?}");

        // A record URI is not a player account: fails before any network call.
        let before = pds.requests().len();
        command_tx
            .send(Command::FetchPlayer {
                subject: "at://did:plc:stubuser/app.bsky.feed.post/3abc".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("want a player account"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            pds.requests().len(),
            before,
            "no HTTP call for a record URI"
        );

        // A repo with no snapshot records reports None.
        command_tx
            .send(Command::FetchPlayer {
                subject: "did:plc:empty".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSnapshot { .. })) {
            Event::PlayerSnapshot {
                subject, snapshot, ..
            } => {
                assert_eq!(subject, "did:plc:empty");
                assert!(snapshot.is_none());
            }
            other => panic!("expected PlayerSnapshot, got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fetch_player_verifies_against_trusted_server_keys() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-player-trusted");
        // A foreign "server" key, trusted via the settings file; the key id
        // is blank in the entry, so the worker derives it from the key.
        let foreign = signing::generate();
        let foreign_id = signing::key_id_of(&foreign.verifying_key());
        let mut config = test_config(pds.url.clone());
        config.trusted_server_keys = vec![crate::config::TrustedServerKey {
            key_id: String::new(),
            public_key: signing::public_key_spki_b64(&foreign.verifying_key()),
        }];
        write_config(&dir, &config);

        // Build a snapshot record signed by `key` (tamper edits it after
        // signing); returns the record and the endorsing key id (the rkey).
        let signed_record = |version: i64, key: &signing::SigningKey, tamper: bool| {
            let key_id = signing::key_id_of(&key.verifying_key());
            let mut record = serde_json::json!({
                "$type": records::SNAPSHOT_COLLECTION,
                "version": version,
                "issuedAt": "2026-11-12T12:00:00.000Z",
                "summary": {
                    "username": "Foreign",
                    "dimension": "minecraft:the_nether",
                    "position": {"x": 1, "y": 2, "z": 3},
                    "health": "20",
                    "food": 20,
                    "xpLevel": 0,
                    "gameMode": "creative",
                },
                "aspects": [],
                "endorsements": [],
            });
            let payload = presence::snapshot_payload("did:plc:stubuser", &record, &key_id).unwrap();
            let sig = signing::sign_b64(key, &payload);
            if tamper {
                record["summary"]["food"] = serde_json::json!(0);
            }
            record["endorsements"] = serde_json::json!([{
                "server": key_id,
                "alg": "ed25519",
                "sig": {"$bytes": sig},
            }]);
            (record, key_id)
        };

        // Logged out: the read is unauthenticated. The trusted key verifies.
        pds.seed_record(
            records::SNAPSHOT_COLLECTION,
            &foreign_id,
            signed_record(3, &foreign, false).0,
        );
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));
        command_tx
            .send(Command::FetchPlayer {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSnapshot { .. })) {
            Event::PlayerSnapshot {
                subject,
                via,
                snapshot,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                let d = snapshot.expect("a snapshot record exists");
                assert_eq!(d.version, 3);
                assert_eq!(d.username, "Foreign");
                assert_eq!(d.dimension, "minecraft:the_nether");
                assert_eq!((d.x, d.y, d.z), (1, 2, 3));
                assert_eq!(d.rkey, foreign_id);
                assert_eq!(
                    d.verdict,
                    records::EndorsementVerdict::Verified {
                        endorser: foreign_id.clone()
                    }
                );
            }
            other => panic!("expected PlayerSnapshot, got {other:?}"),
        }
        let list_hit = pds
            .requests()
            .into_iter()
            .find(|r| r.path.starts_with("/xrpc/com.atproto.repo.listRecords"))
            .expect("listRecords was called");
        assert!(!list_hit.authorized, "the read is unauthenticated");

        // The same record edited after signing: known key, bad signature.
        pds.seed_record(
            records::SNAPSHOT_COLLECTION,
            &foreign_id,
            signed_record(3, &foreign, true).0,
        );
        command_tx
            .send(Command::FetchPlayer {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSnapshot { .. })) {
            Event::PlayerSnapshot { snapshot, .. } => assert_eq!(
                snapshot.unwrap().verdict,
                records::EndorsementVerdict::BadSignature
            ),
            other => panic!("expected PlayerSnapshot, got {other:?}"),
        }

        // A higher-version record endorsed by an untrusted key wins as the
        // latest and reports unknown endorser.
        let stranger = signing::generate();
        let (stranger_record, stranger_id) = signed_record(4, &stranger, false);
        pds.seed_record(records::SNAPSHOT_COLLECTION, &stranger_id, stranger_record);
        command_tx
            .send(Command::FetchPlayer {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSnapshot { .. })) {
            Event::PlayerSnapshot { snapshot, .. } => {
                let d = snapshot.unwrap();
                assert_eq!(d.version, 4);
                assert_eq!(d.verdict, records::EndorsementVerdict::UnknownEndorser);
            }
            other => panic!("expected PlayerSnapshot, got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fetch_player_skin_reads_verifies_and_saves_the_blobs() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-skin");
        write_config(&dir, &test_config(pds.url.clone()));
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        // Publish a skin: the stub stores the putRecord body and uploadBlob
        // answers with STUB_CIDS[0] as the skin blob's CID.
        let skin = dir.join("skin.png");
        std::fs::write(&skin, tiny_png()).unwrap();
        command_tx
            .send(Command::PostSkin {
                skin_path: Some(skin.display().to_string()),
                cape_path: None,
                model: "slim".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));

        // Read it back by handle (the stub doubles as the handle service).
        command_tx
            .send(Command::FetchPlayerSkin {
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSkin { .. })) {
            Event::PlayerSkin { subject, via, skin } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                let d = skin.expect("a skin record exists");
                assert_eq!(d.model, "slim");
                let key_file: signing::SigningKeyFile = serde_json::from_str(
                    &std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap(),
                )
                .unwrap();
                assert_eq!(d.rkey, key_file.key_id);
                assert_eq!(
                    d.verdict,
                    records::EndorsementVerdict::Verified {
                        endorser: key_file.key_id.clone()
                    }
                );
                // The verified blob was downloaded and saved under skins/.
                let skin_blob = d.skin.as_ref().expect("a skin blob");
                assert_eq!(skin_blob.cid, STUB_CIDS[0]);
                assert_eq!(skin_blob.mime_type, "image/png");
                let path = skin_blob.path.as_ref().expect("verified blob is saved");
                assert!(path.contains(SKINS_DIR), "{path}");
                assert!(path.ends_with(&format!("{}.png", STUB_CIDS[0])), "{path}");
                assert_eq!(std::fs::read(path).unwrap(), tiny_png());
                assert!(d.cape.is_none());
            }
            other => panic!("expected PlayerSkin, got {other:?}"),
        }
        // The listRecords and getBlob calls went out unauthenticated.
        let requests = pds.requests();
        let list_hit = requests
            .iter()
            .find(|r| r.path.starts_with("/xrpc/com.atproto.repo.listRecords"))
            .expect("listRecords was called");
        assert!(!list_hit.authorized, "the read is unauthenticated");
        assert!(list_hit.path.contains("collection=at.atpcraft.player.skin"));
        let blob_hit = requests
            .iter()
            .find(|r| r.path.starts_with("/xrpc/com.atproto.sync.getBlob"))
            .expect("getBlob was called");
        assert!(!blob_hit.authorized, "the blob read is unauthenticated");
        assert!(blob_hit.path.contains(&format!("cid={}", STUB_CIDS[0])));

        // A record URI is not a player account: fails before any network call.
        let before = pds.requests().len();
        command_tx
            .send(Command::FetchPlayerSkin {
                subject: "at://did:plc:stubuser/app.bsky.feed.post/3abc".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("want a player account"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            pds.requests().len(),
            before,
            "no HTTP call for a record URI"
        );

        // A repo with no skin records reports None.
        command_tx
            .send(Command::FetchPlayerSkin {
                subject: "did:plc:empty".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSkin { .. })) {
            Event::PlayerSkin { subject, skin, .. } => {
                assert_eq!(subject, "did:plc:empty");
                assert!(skin.is_none());
            }
            other => panic!("expected PlayerSkin, got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fetch_player_skin_trusts_configured_keys_and_skips_unverified_blobs() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-skin-trusted");
        // A foreign "server" key, trusted via the settings file (blank key id
        // in the entry, so the worker derives it from the public key).
        let foreign = signing::generate();
        let foreign_id = signing::key_id_of(&foreign.verifying_key());
        let mut config = test_config(pds.url.clone());
        config.trusted_server_keys = vec![crate::config::TrustedServerKey {
            key_id: String::new(),
            public_key: signing::public_key_spki_b64(&foreign.verifying_key()),
        }];
        write_config(&dir, &config);

        // A skin record signed by `key`; `tamper` edits the model after
        // signing. Returns the record and the endorsing key id (the rkey).
        let signed_record = |key: &signing::SigningKey, tamper: bool| {
            let key_id = signing::key_id_of(&key.verifying_key());
            let mut record = serde_json::json!({
                "$type": records::SKIN_COLLECTION,
                "skin": {
                    "$type": "blob",
                    "ref": {"$link": STUB_CIDS[1]},
                    "mimeType": "image/png",
                    "size": 42,
                },
                "model": "default",
                "timestamp": "2026-11-12T12:00:00.000Z",
                "endorsements": [],
            });
            let payload = skins::skin_payload("did:plc:stubuser", &record, &key_id).unwrap();
            let sig = signing::sign_b64(key, &payload);
            if tamper {
                record["model"] = serde_json::json!("slim");
            }
            record["endorsements"] = serde_json::json!([{
                "server": key_id,
                "alg": "ed25519",
                "sig": {"$bytes": sig},
            }]);
            (record, key_id)
        };

        // Logged out: the reads are unauthenticated and the trusted key
        // verifies, so the blob is downloaded and saved.
        let (record, rkey) = signed_record(&foreign, false);
        pds.seed_record(records::SKIN_COLLECTION, &rkey, record);
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));
        command_tx
            .send(Command::FetchPlayerSkin {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSkin { .. })) {
            Event::PlayerSkin { subject, via, skin } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                let d = skin.expect("a skin record exists");
                assert_eq!(d.rkey, foreign_id);
                assert_eq!(d.model, "default");
                assert_eq!(
                    d.verdict,
                    records::EndorsementVerdict::Verified {
                        endorser: foreign_id.clone()
                    }
                );
                let skin_blob = d.skin.as_ref().expect("a skin blob");
                assert_eq!(skin_blob.cid, STUB_CIDS[1]);
                assert!(skin_blob.path.is_some(), "verified blob is saved");
            }
            other => panic!("expected PlayerSkin, got {other:?}"),
        }
        let blob_calls = || {
            pds.requests()
                .iter()
                .filter(|r| r.path.starts_with("/xrpc/com.atproto.sync.getBlob"))
                .count()
        };
        assert_eq!(blob_calls(), 1, "the verified blob was downloaded");

        // The same record edited after signing: known key, bad signature, and
        // no blob is downloaded for it.
        let (record, _) = signed_record(&foreign, true);
        pds.seed_record(records::SKIN_COLLECTION, &foreign_id, record);
        command_tx
            .send(Command::FetchPlayerSkin {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::PlayerSkin { .. })) {
            Event::PlayerSkin { skin, .. } => {
                let d = skin.expect("a skin record exists");
                assert_eq!(d.verdict, records::EndorsementVerdict::BadSignature);
                assert!(
                    d.skin.as_ref().map(|b| b.path.is_none()).unwrap_or(false),
                    "unverified blobs are not persisted"
                );
            }
            other => panic!("expected PlayerSkin, got {other:?}"),
        }
        assert_eq!(blob_calls(), 1, "no getBlob for an unverified record");

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn startup_resumes_a_persisted_session_without_login() {
        let pds = StubPds::start(false);
        let dir = temp_dir("resume");
        write_config(&dir, &test_config(pds.url.clone()));
        // A session persisted by a previous run.
        let stored = session::StoredSession {
            pds_url: pds.url.clone(),
            did: "did:plc:stubuser".into(),
            handle: "stub.test".into(),
            refresh_jwt: "stub-refresh".into(),
            oauth: None,
        };
        std::fs::write(
            dir.join(SESSION_FILE),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        // No Login command: the startup resume logs in on its own.
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        }) {
            Event::Session(SessionState::LoggedIn { did }) => {
                assert_eq!(did, "did:plc:stubuser");
            }
            other => panic!("expected Session(LoggedIn), got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        // The PDS saw a refreshSession with the stored refresh token, and the
        // rotated token was persisted.
        let requests = pds.requests();
        assert!(
            requests
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.server.refreshSession")
        );
        assert!(
            !requests
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.server.createSession"),
            "no password login happened"
        );
        let saved: session::StoredSession =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SESSION_FILE)).unwrap())
                .unwrap();
        assert_eq!(saved.refresh_jwt, "stub-refresh-2", "the rotated token");
    }

    #[test]
    fn startup_reports_a_failed_resume_instead_of_staying_silent() {
        let dir = temp_dir("resume-fails");
        // A persisted session whose PDS is unreachable (port 1 refuses).
        let stored = session::StoredSession {
            pds_url: "http://127.0.0.1:1".into(),
            did: "did:plc:stubuser".into(),
            handle: "stub.test".into(),
            refresh_jwt: "stub-refresh".into(),
            oauth: None,
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(SESSION_FILE),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        // #18: the resume failure surfaces as an event, not silence.
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::Failed { .. }))
        }) {
            Event::Session(SessionState::Failed { message }) => {
                assert!(
                    message.contains("could not resume the stored session"),
                    "{message}"
                );
            }
            other => panic!("expected Session(Failed), got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        // The persisted file is kept so a later start can retry.
        assert!(dir.join(SESSION_FILE).exists());
    }

    #[test]
    fn logout_clears_the_session_and_the_persisted_file() {
        let pds = StubPds::start(false);
        let dir = temp_dir("logout");
        write_config(&dir, &test_config(pds.url.clone()));

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        assert!(
            dir.join(SESSION_FILE).exists(),
            "login persisted the session"
        );

        command_tx.send(Command::Logout).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedOut))
        });
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        assert!(!dir.join(SESSION_FILE).exists(), "logout deleted the file");
    }

    #[test]
    fn death_message_posts_only_when_enabled() {
        let pds = StubPds::start(false);
        let dir = temp_dir("death-off");
        let mut config = test_config(pds.url.clone());
        config.post_death_messages = false;
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx
            .send(Command::PostDeathMessage {
                text: "Player was slain by Zombie".into(),
            })
            .unwrap();
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        assert!(
            !pds.requests()
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.repo.createRecord"),
            "flag off: nothing was posted"
        );

        // Flag on: the death message is posted as a text post.
        let pds = StubPds::start(false);
        let dir = temp_dir("death-on");
        let mut config = test_config(pds.url.clone());
        config.post_death_messages = true;
        write_config(&dir, &config);
        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx
            .send(Command::PostDeathMessage {
                text: "Player was slain by Zombie".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        let requests = pds.requests();
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .expect("the death message was posted");
        let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
        assert_eq!(body["collection"], "app.bsky.feed.post");
        assert_eq!(body["record"]["text"], "☠ Player was slain by Zombie");
    }

    #[test]
    fn login_without_a_settings_file_reports_not_configured() {
        let dir = temp_dir("no-config");
        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::Login).unwrap();
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::Failed { .. }))
        }) {
            Event::Session(SessionState::Failed { message }) => {
                assert!(message.contains("could not read the settings file"));
            }
            other => panic!("expected Session(Failed), got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    // -----------------------------------------------------------------------
    // M8: localhost OAuth login
    // -----------------------------------------------------------------------

    use crate::oauth::tests::{StubAuthServer, follow_authorize_redirect};

    /// Spawn a worker whose handle/DID resolution points at the stub.
    fn spawn_worker_at(
        storage_dir: PathBuf,
        stub: &StubAuthServer,
    ) -> (
        mpsc::Sender<Command>,
        mpsc::Receiver<Event>,
        std::thread::JoinHandle<()>,
    ) {
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let options = WorkerOptions {
            storage_dir,
            resolver: stub.resolver(),
            launch_browser: false,
        };
        let handle = spawn(command_rx, event_tx, options);
        (command_tx, event_rx, handle)
    }

    /// Drive the browser side of an OAuth login: the worker emitted the
    /// authorize URL; follow it and land on the loopback listener.
    fn simulate_browser(event_rx: &mpsc::Receiver<Event>) -> String {
        match wait_for(event_rx, |e| matches!(e, Event::OAuthAuthorize { .. })) {
            Event::OAuthAuthorize { url } => {
                let status = follow_authorize_redirect(&url);
                assert!(
                    status.contains("200"),
                    "the loopback page accepts the redirect: {status}"
                );
                url
            }
            other => panic!("expected OAuthAuthorize, got {other:?}"),
        }
    }

    #[test]
    fn login_oauth_full_flow_then_post_text_with_dpop() {
        let stub = StubAuthServer::start(false);
        let dir = temp_dir("oauth-login");
        let (command_tx, event_rx, handle) = spawn_worker_at(dir.clone(), &stub);

        command_tx
            .send(Command::LoginOAuth {
                handle: "stub.test".into(),
            })
            .unwrap();
        // LoggingIn first, then the authorize URL for the browser.
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggingIn))
        });
        let authorize_url = simulate_browser(&event_rx);
        assert!(
            authorize_url.starts_with(&format!("{}/authorize?", stub.url)),
            "the authorize URL points at the discovered authorization server"
        );
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        }) {
            Event::Session(SessionState::LoggedIn { did }) => assert_eq!(did, "did:plc:stubuser"),
            other => panic!("expected Session(LoggedIn), got {other:?}"),
        }

        // A post goes out with `Authorization: DPoP …` and a proof (the stub
        // verifies both and would panic the connection thread otherwise).
        command_tx
            .send(Command::PostText {
                text: "hello over oauth".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                assert_eq!(uri, "at://did:plc:stubuser/app.bsky.feed.post/stubrec")
            }
            other => panic!("expected Posted, got {other:?}"),
        }

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let requests = stub.requests();
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .expect("createRecord was called");
        assert_eq!(
            create_record.authorization.as_deref(),
            Some("DPoP stub-oauth-access-1")
        );
        assert!(create_record.dpop.is_some(), "a DPoP proof was sent");
        let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
        assert_eq!(body["record"]["text"], "hello over oauth");

        // The session file carries the OAuth block (DPoP key + token endpoint),
        // and never an app password.
        let raw = std::fs::read_to_string(dir.join(SESSION_FILE)).unwrap();
        let stored: session::StoredSession = serde_json::from_str(&raw).unwrap();
        let oauth = stored.oauth.expect("the OAuth block is persisted");
        assert_eq!(oauth.token_endpoint, format!("{}/token", stub.url));
        assert_eq!(stored.refresh_jwt, "stub-oauth-refresh-1");
        assert!(!raw.contains("password"));
        // The persisted DPoP key rebuilds.
        assert!(oauth::DpopKey::from_jwk_json(&oauth.dpop_jwk).is_ok());
        // session.json holds the DPoP private key: owner-only on unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(SESSION_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "session.json must be 0600");
        }
    }

    #[test]
    #[cfg(unix)]
    fn write_private_makes_the_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("private-perms");
        let path = dir.join("secret.json");
        write_private(&path, b"{}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Rewriting keeps the restrictive mode.
        write_private(&path, b"{\"a\":1}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn startup_resumes_a_persisted_oauth_session_via_dpop_refresh() {
        let stub = StubAuthServer::start(false);
        let dir = temp_dir("oauth-resume");
        // A session.json as written by a previous OAuth login.
        let key = oauth::DpopKey::generate();
        let stored = session::StoredSession {
            pds_url: stub.url.clone(),
            did: "did:plc:stubuser".into(),
            handle: "stub.test".into(),
            refresh_jwt: "stub-oauth-refresh-1".into(),
            oauth: Some(session::StoredOAuth {
                dpop_jwk: key.to_jwk_json(),
                token_endpoint: format!("{}/token", stub.url),
                issuer: stub.url.clone(),
                client_id: oauth::client_id(oauth::DEFAULT_SCOPE),
                scope: oauth::DEFAULT_SCOPE.into(),
            }),
        };
        std::fs::write(
            dir.join(SESSION_FILE),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();

        let (command_tx, event_rx, handle) = spawn_worker_at(dir.clone(), &stub);
        // No Login command: the startup resume logs in on its own.
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        }) {
            Event::Session(SessionState::LoggedIn { did }) => assert_eq!(did, "did:plc:stubuser"),
            other => panic!("expected Session(LoggedIn), got {other:?}"),
        }

        // Posting works straight away, with the refreshed token.
        command_tx
            .send(Command::PostText {
                text: "after resume".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let requests = stub.requests();
        // The refresh grant went to the token endpoint with the stored
        // DPoP-bound refresh token…
        let token = requests
            .iter()
            .find(|r| r.path == "/token")
            .expect("the token endpoint was called");
        assert!(token.body.contains("grant_type=refresh_token"));
        assert!(token.body.contains("stub-oauth-refresh-1"));
        assert!(token.dpop.is_some(), "the refresh is DPoP-signed");
        // …no app-password createSession happened…
        assert!(
            !requests
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.server.createSession"),
            "no password login happened"
        );
        // …and the post used the rotated access token.
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .unwrap();
        assert_eq!(
            create_record.authorization.as_deref(),
            Some("DPoP stub-oauth-access-2")
        );
        // The rotated refresh token was persisted.
        let saved: session::StoredSession =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SESSION_FILE)).unwrap())
                .unwrap();
        assert_eq!(saved.refresh_jwt, "stub-oauth-refresh-2");
    }

    #[test]
    fn login_oauth_with_a_mismatched_redirect_state_fails() {
        let stub = StubAuthServer::start(true); // wrong_state
        let dir = temp_dir("oauth-bad-state");
        let (command_tx, event_rx, handle) = spawn_worker_at(dir.clone(), &stub);

        command_tx
            .send(Command::LoginOAuth {
                handle: "stub.test".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggingIn))
        });
        match wait_for(&event_rx, |e| matches!(e, Event::OAuthAuthorize { .. })) {
            Event::OAuthAuthorize { url } => {
                let status = follow_authorize_redirect(&url);
                assert!(
                    status.contains("400"),
                    "the bad state is rejected: {status}"
                );
            }
            other => panic!("expected OAuthAuthorize, got {other:?}"),
        }
        match wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::Failed { .. }))
        }) {
            Event::Session(SessionState::Failed { message }) => {
                assert!(message.contains("state"), "{message}")
            }
            other => panic!("expected Session(Failed), got {other:?}"),
        }
        // No session file was written for the failed login.
        assert!(!std::path::Path::new(&dir.join(SESSION_FILE)).exists());
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn shutdown_stays_responsive_while_an_oauth_login_waits_for_the_browser() {
        let stub = StubAuthServer::start(false);
        let dir = temp_dir("oauth-shutdown");
        let (command_tx, event_rx, handle) = spawn_worker_at(dir, &stub);

        command_tx
            .send(Command::LoginOAuth {
                handle: "stub.test".into(),
            })
            .unwrap();
        // The authorize URL arrives; nobody completes the redirect.
        let _ = wait_for(&event_rx, |e| matches!(e, Event::OAuthAuthorize { .. }));
        // Shutdown is honoured immediately even though the login task is
        // still waiting on the loopback listener.
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
        assert!(event_rx.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
