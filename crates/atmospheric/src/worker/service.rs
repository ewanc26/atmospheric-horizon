//! Background worker thread: the only place network and disk I/O happens.
//!
//! The game side never awaits anything. It pushes [`Command`]s through one
//! channel and drains [`Event`]s from another once per tick; this thread owns
//! the tokio runtime, the AT Protocol agent, and the settings file under the
//! mod's own storage folder. Keeping that boundary is what makes the rest of
//! the crate testable without a game.

use crate::{
    Command, Config, Event, SweepPlayer, actions, chats,
    colibri::{self, ChannelRef, StreamEvent},
    error::AtmosphericError,
    graph, identity, interactions,
    notifications::{self, SeenCursor},
    oauth::{self, DpopState, SharedDpop},
    presence::{self, PlayerDigest},
    profiles,
    records::{self, SnapshotRecord},
    servers,
    session::{self, Agent, SessionState, StoredSession},
    signing::{self, SigningKey},
    skins,
    tid::TidGen,
};
use ed25519_dalek::VerifyingKey;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
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
/// The separate server account's persisted session (M25), same folder.
pub const SERVER_SESSION_FILE: &str = "server-session.json";
/// Where verified skin/cape blobs downloaded by M14 reads are saved.
pub const SKINS_DIR: &str = "skins";

/// How long the presence fallback waits for the game or a chat mod to emit a
/// join/leave line (M29), matching atpcraft's 3-second wait.
const COLIBRI_PRESENCE_WAIT: Duration = Duration::from_secs(3);

/// atpcraft's player message limit: at most this many player-authored writes
/// in a rolling window.
const COLIBRI_SEND_LIMIT: usize = 20;
/// The rolling window for [`COLIBRI_SEND_LIMIT`].
const COLIBRI_SEND_WINDOW: Duration = Duration::from_secs(60);
/// How far around the connection event the fallback looks for a handled line
/// (M29), matching atpcraft's 5-second window.
const COLIBRI_PRESENCE_WINDOW: Duration = Duration::from_secs(5);

/// How long the M38 login gate waits for the identity map and list read.
const JOIN_GATE_TIMEOUT: Duration = Duration::from_secs(5);
/// How many of the newest generic records the HUD shows (M44), matching the
/// M16 chat-history limit.
const RECORDS_HISTORY_LIMIT: usize = 5;

/// How long the M42 online sweep waits for the list refresh and the per-player
/// link lookups. Longer than one login gate because it covers every online
/// player; a timeout kicks nobody.
const JOIN_GATE_SWEEP_TIMEOUT: Duration = Duration::from_secs(30);
const JOIN_GATE_NO_LIST: &str = "This server's Bluesky join gate has no whitelist list configured.";
const JOIN_GATE_UNLINKED: &str =
    "This server requires a linked Bluesky account. Contact the server operator.";
const JOIN_GATE_NOT_MEMBER: &str = "You are not on this server's Bluesky whitelist.";
const JOIN_GATE_UNAVAILABLE: &str =
    "Could not verify Bluesky whitelist membership. Try again shortly.";

/// The only allow is a linked DID on the configured list (M38).
enum JoinGateVerdict {
    Allowed,
    NotMember,
    Unlinked(String),
}

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
                // the loop below can select on the notification timer (M4)
                // next to the commands. When the game side drops its sender,
                // this thread ends, `rx` yields `None`, and the loop exits.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
                // Results of spawned OAuth logins (M8) come back here, so the
                // loop stays responsive while the browser is open.
                let (oauth_tx, mut oauth_rx) = tokio::sync::mpsc::unbounded_channel::<(
                    OAuthTarget,
                    crate::error::Result<oauth::OAuthLogin>,
                )>();
                let (colibri_tx, mut colibri_rx) =
                    tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
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

                let mut worker = Worker::new(options, events, oauth_tx, colibri_tx);
                worker.emit(Event::Session(SessionState::LoggedOut));
                worker.emit_join_gate_state();
                // Resume persisted player and server sessions (refresh
                // tokens) if there are any.
                worker.try_resume().await;
                worker.try_resume_server().await;
                // The notification poll timer (M4): ticks at the configured
                // interval once logged in; before that the ticks are no-ops.
                let mut poll =
                    tokio::time::interval(Duration::from_secs(worker.poll_interval_secs));
                poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The server-account token refresh (M50): the first check runs
                // after one period, matching atpcraft's scheduled refresher.
                let refresh_period = server_token_refresh_period(worker.server_token_refresh_secs);
                let mut server_token_refresh = tokio::time::interval_at(
                    tokio::time::Instant::now() + refresh_period,
                    refresh_period,
                );
                server_token_refresh
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
                                Some((target, Ok(login))) => {
                                    worker.finish_login_oauth(target, login).await;
                                    if worker.is_logged_in() {
                                        poll = tokio::time::interval(Duration::from_secs(
                                            worker.poll_interval_secs,
                                        ));
                                        poll.set_missed_tick_behavior(
                                            tokio::time::MissedTickBehavior::Delay,
                                        );
                                    }
                                    if target == OAuthTarget::Server
                                        && worker.server_agent.is_some()
                                    {
                                        let period = server_token_refresh_period(
                                            worker.server_token_refresh_secs,
                                        );
                                        server_token_refresh = tokio::time::interval_at(
                                            tokio::time::Instant::now() + period,
                                            period,
                                        );
                                        server_token_refresh.set_missed_tick_behavior(
                                            tokio::time::MissedTickBehavior::Delay,
                                        );
                                    }
                                }
                                Some((target, Err(error))) => match target {
                                    OAuthTarget::Player => worker.emit(Event::Session(
                                        SessionState::Failed {
                                            message: error.to_string(),
                                        },
                                    )),
                                    OAuthTarget::Server => worker.emit_server_account(
                                        SessionState::Failed {
                                            message: error.to_string(),
                                        },
                                    ),
                                },
                                None => {}
                            }
                        }
                        update = colibri_rx.recv() => {
                            if let Some(update) = update {
                                worker.handle_colibri_update(update).await;
                            }
                        }
                        _ = poll.tick() => {
                            worker.poll_notifications(false).await;
                        }
                        _ = server_token_refresh.tick() => {
                            worker.refresh_server_token().await;
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

/// Which session a finished OAuth login belongs to (M25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OAuthTarget {
    Player,
    Server,
}

/// Cached member DIDs for one list/AppView pair (M52).
struct ListCache {
    appview: String,
    list: String,
    members: Vec<String>,
    fetched_at: Instant,
}

impl ListCache {
    /// A cache entry is fresh only for the same AppView and canonical list,
    /// and strictly inside the TTL. `0` therefore always refreshes.
    fn is_fresh(&self, appview: &str, list: &str, ttl_seconds: u64, now: Instant) -> bool {
        self.appview == appview
            && self.list == list
            && now.saturating_duration_since(self.fetched_at) < Duration::from_secs(ttl_seconds)
    }
}

/// The worker's state: the agent and DID once logged in, plus its files.
struct Worker {
    storage_dir: PathBuf,
    agent: Option<Agent>,
    did: Option<String>,
    cursor: SeenCursor,
    poll_interval_secs: u64,
    /// Seconds between proactive server-account token refreshes (M50).
    server_token_refresh_secs: u64,
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
    /// The separate server account's agent, DID and handle (M25).
    server_agent: Option<Agent>,
    server_did: Option<String>,
    server_handle: Option<String>,
    /// The persisted server-account session, if any.
    server_stored_session: Option<StoredSession>,
    /// DPoP state for the server account, separate from the player's.
    server_dpop: SharedDpop,
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
    /// Cached list members for M52.
    list_cache: Option<ListCache>,
    /// Where handles/DIDs resolve for OAuth logins.
    resolver: oauth::Resolver,
    /// Whether OAuth logins may spawn the system browser.
    launch_browser: bool,
    /// Where spawned OAuth logins report their result and target.
    oauth_tx:
        tokio::sync::mpsc::UnboundedSender<(OAuthTarget, crate::error::Result<oauth::OAuthLogin>)>,
    events: Sender<Event>,
    /// The socket loop reports connection and message events here.
    colibri_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    colibri_channel: Option<ChannelRef>,
    colibri_connected: bool,
    colibri_task: Option<tokio::task::JoinHandle<()>>,
    /// AT-URIs this worker wrote, so the stream echo does not repeat them.
    colibri_sent: VecDeque<String>,
    /// The last join, leave and blank/hidden lines used by the M29 presence
    /// fallback, with the same 5-second window semantics as atpcraft.
    colibri_last_join: Option<Instant>,
    colibri_last_leave: Option<Instant>,
    colibri_last_blank: Option<Instant>,
    /// When the current bridge generation started. A fallback scheduled before
    /// a restart must not write into the new channel.
    colibri_started_at: Option<Instant>,
    /// Whether the local player's Colibri profile was checked for this bridge
    /// session (M30).
    colibri_profile_checked: bool,
    /// Whether the local player's own Colibri membership was repaired for this
    /// bridge session (M32).
    colibri_player_membership_checked: bool,
    /// Player-authored Colibri messages inside the rolling send window (M31).
    colibri_send_times: VecDeque<Instant>,
    /// The DID the current `colibri_send_times` window belongs to.
    colibri_rate_did: Option<String>,
}

/// Whether a due connection-event fallback still needs a notice. Only the last
/// line of each kind matters; a blank line counts for both join and leave.
fn presence_notice_needed(
    last_join: Option<Instant>,
    last_leave: Option<Instant>,
    last_blank: Option<Instant>,
    kind: &str,
    at: Instant,
) -> bool {
    let last_kind = match kind {
        "join" => last_join,
        "leave" => last_leave,
        _ => return false,
    };
    let within = |time: Option<Instant>| {
        time.is_some_and(|time| {
            time <= at + COLIBRI_PRESENCE_WINDOW && at <= time + COLIBRI_PRESENCE_WINDOW
        })
    };
    !within(last_kind) && !within(last_blank)
}

/// Whether one more player-authored Colibri write fits in the rolling window.
/// Expired timestamps are dropped first, so a send at exactly 60 seconds is
/// allowed. This mirrors atpcraft's `ColibriRateLimiter`.
fn colibri_send_allowed(hits: &mut VecDeque<Instant>, now: Instant) -> bool {
    while hits
        .front()
        .is_some_and(|hit| now.duration_since(*hit) >= COLIBRI_SEND_WINDOW)
    {
        hits.pop_front();
    }
    if hits.len() >= COLIBRI_SEND_LIMIT {
        return false;
    }
    hits.push_back(now);
    true
}

/// The server profile's display name (M49): `server_name`, or atpcraft's
/// client-name fallback when it is blank.
fn server_profile_name(config: &Config) -> String {
    if config.server_name.is_empty() {
        config.client_name.clone()
    } else {
        config.server_name.clone()
    }
}

/// The server profile's capabilities (M49): an explicit override wins;
/// otherwise derive the features that are actually implemented. `skin-sync`
/// is always present (M9/M14); `join-gate` appears when the gate is armed.
/// `player-data-sync` and `labeler` stay absent until #111/#112.
fn server_profile_capabilities(config: &Config) -> Vec<String> {
    if !config.server_capabilities.is_empty() {
        return config.server_capabilities.clone();
    }
    let mut capabilities = Vec::new();
    if config.join_gate_enabled {
        capabilities.push("join-gate".into());
    }
    capabilities.push("skin-sync".into());
    capabilities
}

/// The M50 server-token refresh period, floored at one second like atpcraft.
fn server_token_refresh_period(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

impl Worker {
    fn new(
        options: WorkerOptions,
        events: Sender<Event>,
        oauth_tx: tokio::sync::mpsc::UnboundedSender<(
            OAuthTarget,
            crate::error::Result<oauth::OAuthLogin>,
        )>,
        colibri_tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
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
        let server_stored_session =
            std::fs::read_to_string(options.storage_dir.join(SERVER_SESSION_FILE))
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok());
        Self {
            storage_dir: options.storage_dir,
            agent: None,
            did: None,
            cursor,
            poll_interval_secs: Config::default().poll_interval_secs,
            server_token_refresh_secs: Config::default()
                .server_account_token_refresh_seconds
                .max(1),
            tid: TidGen::new(),
            post_chat_messages: false,
            post_death_messages: false,
            persist_session: true,
            stored_session,
            server_agent: None,
            server_did: None,
            server_handle: None,
            server_stored_session,
            server_dpop: oauth::new_shared_dpop(),
            chat_server_id: Config::default().chat_server_id,
            identifier: String::new(),
            signing_key: None,
            snapshot_version,
            dpop: oauth::new_shared_dpop(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            list_cache: None,
            resolver: options.resolver.clone(),
            launch_browser: options.launch_browser,
            oauth_tx,
            events,
            colibri_tx,
            colibri_channel: None,
            colibri_connected: false,
            colibri_task: None,
            colibri_sent: VecDeque::new(),
            colibri_last_join: None,
            colibri_last_leave: None,
            colibri_last_blank: None,
            colibri_started_at: None,
            colibri_profile_checked: false,
            colibri_player_membership_checked: false,
            colibri_send_times: VecDeque::new(),
            colibri_rate_did: None,
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
                } else {
                    self.emit_join_gate_state();
                }
            }
            Command::Login => self.login().await,
            Command::LoginOAuth { handle } => self.start_login_oauth(handle),
            Command::LoginServerOAuth { handle } => self.start_server_login_oauth(handle),
            Command::ServerAccountStatus => self.emit_server_account_status(),
            Command::LogoutServerAccount => self.logout_server_account(),
            Command::ColibriStart => self.start_colibri().await,
            Command::ColibriReconnect => self.reconnect_colibri().await,
            Command::ColibriStop => self.stop_colibri(true),
            Command::ColibriStatus => self.emit_colibri(None),
            Command::ColibriSend { text, automatic } => self.send_colibri(&text, automatic).await,
            Command::ColibriNotice { text } => self.send_colibri_notice(&text).await,
            Command::ColibriHandled { kind } => self.mark_colibri_handled(kind.as_deref()),
            Command::ColibriPresence { kind, text } => self.schedule_colibri_fallback(&kind, text),
            Command::ColibriPlayerJoined {
                player_uuid,
                player_name,
            } => {
                self.check_remote_colibri_profile(&player_uuid, &player_name)
                    .await
            }
            Command::CheckPlayerProfile {
                player_uuid,
                player_name,
            } => self.check_player_profile(&player_uuid, &player_name).await,
            Command::ClaimServerLink { code } => self.claim_server_link(&code).await,
            Command::StartBrowserLink {
                player_uuid,
                player_name,
            } => self.start_browser_link(&player_uuid, &player_name).await,
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
            Command::PostServerProfileAsServer => {
                match self.post_server_profile_as_server().await {
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
            Command::CheckGate { list, subject } => {
                // Public AppView read: works logged out, no token refresh.
                // Fail-closed: an error is the deny-with-reason.
                match self.check_gate(&list, &subject).await {
                    Ok(outcome) => self.emit(Event::GateDecision {
                        list: outcome.list,
                        subject: outcome.subject,
                        allowed: outcome.allowed,
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::JoinGateCheck {
                request_id,
                player_uuid,
                player_name,
            } => {
                self.check_join_gate(request_id, &player_uuid, &player_name)
                    .await;
            }
            Command::JoinGateSweep { players } => {
                self.join_gate_sweep(players).await;
            }
            Command::FetchChat { subject } => {
                // A public read against the subject's own PDS: works logged
                // out, no token refresh.
                match self.fetch_chat(&subject).await {
                    Ok((subject, via, total, items)) => self.emit(Event::ChatHistory {
                        subject,
                        via,
                        total,
                        items,
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchRecords {
                subject,
                collection,
            } => {
                // A public read against the subject's own PDS: works logged
                // out, no token refresh.
                match self.fetch_records(&subject, &collection).await {
                    Ok((subject, via, collection, total, items)) => {
                        self.emit(Event::RecordsHistory {
                            subject,
                            via,
                            collection,
                            total,
                            items,
                        })
                    }
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
            Command::FetchProfile { subject } => {
                // Public AppView and PDS reads: works logged out, no token
                // refresh.
                match self.fetch_profile(&subject).await {
                    Ok((subject, via, profile)) => self.emit(Event::Profile {
                        subject,
                        via,
                        profile: Box::new(profile),
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
            Command::FetchServerProfile { subject } => {
                // A public read against the subject's own PDS: works logged
                // out, no token refresh.
                match self.fetch_server_profile(&subject).await {
                    Ok((subject, via, profile)) => self.emit(Event::ServerProfile {
                        subject,
                        via,
                        profile: Box::new(profile),
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::FetchGraph { kind, subject } => {
                // Public AppView read: works logged out, no token refresh.
                match self.fetch_graph(&kind, &subject).await {
                    Ok((kind, subject, via, members)) => self.emit(Event::SocialGraph {
                        kind,
                        subject,
                        via,
                        members,
                    }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::GraphWrite { action, subject } => {
                self.ensure_oauth_fresh().await;
                match self.graph_write(&action, &subject).await {
                    Ok((action, subject)) => self.emit(Event::GraphChanged { action, subject }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::Reply { uri, text } => {
                self.ensure_oauth_fresh().await;
                match self.reply(&uri, &text).await {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::Like { uri } => {
                self.ensure_oauth_fresh().await;
                match self.like(&uri).await {
                    Ok(uri) => self.emit(Event::Posted { uri }),
                    Err(error) => self.fail(error.to_string()),
                }
            }
            Command::Repost { uri } => {
                self.ensure_oauth_fresh().await;
                match self.repost(&uri).await {
                    Ok(uri) => self.emit(Event::Posted { uri }),
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
                self.colibri_profile_checked = false;
                self.colibri_player_membership_checked = false;
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
                // Resolve the server id before borrowing the agent: with no
                // override this loads or creates the M6 signing key.
                let server_id = if self.is_logged_in() {
                    match self.chat_record_server_id() {
                        Ok(server_id) => server_id,
                        Err(error) => {
                            self.fail(error.to_string());
                            return;
                        }
                    }
                } else {
                    String::new()
                };
                let result = match (&self.agent, &self.did) {
                    (Some(agent), Some(did)) => {
                        actions::post_chat(agent, did, &server_id, &message, &mut self.tid).await
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

    /// The Ed25519 endorsement key, loaded or created once for this worker.
    fn endorsement_key(&mut self) -> crate::error::Result<SigningKey> {
        match &self.signing_key {
            Some(key) => Ok(key.clone()),
            None => {
                let key = self.load_or_create_signing_key()?;
                self.signing_key = Some(key.clone());
                Ok(key)
            }
        }
    }

    /// The `server` value for chat records (M48): the configured override, or
    /// the persisted Ed25519 key id atpcraft uses. Loads or creates the key on
    /// first use, so a chat-only install does not need a snapshot first.
    fn chat_record_server_id(&mut self) -> crate::error::Result<String> {
        if !self.chat_server_id.trim().is_empty() {
            return Ok(self.chat_server_id.clone());
        }
        let key = self.endorsement_key()?;
        Ok(signing::key_id_of(&key.verifying_key()))
    }

    /// Publish/refresh the player account's server profile
    /// (`at.atpcraft.server.profile` at rkey `self`, M10, atpcraft port).
    async fn post_server_profile(&mut self) -> crate::error::Result<String> {
        let key = self.endorsement_key()?;
        let config = self.load_config().unwrap_or_default();
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let handle = self.identifier.clone();
        Self::write_server_profile(agent, did, &handle, &key, &config).await
    }

    /// Publish/refresh the server profile from the separate server account
    /// (M25), as atpcraft's server owns its descriptor.
    async fn post_server_profile_as_server(&mut self) -> crate::error::Result<String> {
        self.ensure_server_oauth_fresh().await;
        let key = self.endorsement_key()?;
        let config = self.load_config().unwrap_or_default();
        let (Some(agent), Some(did), Some(handle)) =
            (&self.server_agent, &self.server_did, &self.server_handle)
        else {
            return Err(AtmosphericError::Message(
                "no server account is logged in — run /atmospheric server-account login <handle>"
                    .into(),
            ));
        };
        Self::write_server_profile(agent, did, handle, &key, &config).await
    }

    /// Re-publish the descriptor after a server-account login or a startup
    /// resume (M51), as atpcraft does. Best-effort: a failed write is reported
    /// but never undoes the session.
    async fn auto_publish_server_profile(&mut self) {
        if let Err(error) = self.post_server_profile_as_server().await {
            self.fail(format!("could not publish the server profile: {error}"));
        }
    }

    /// Shared server-profile writer. Identity fields and capabilities come
    /// from the settings file; the `signingKey` block is the mod's persisted
    /// Ed25519 key; `createdAt` is preserved from the existing record.
    async fn write_server_profile(
        agent: &Agent,
        did: &str,
        account_handle: &str,
        key: &SigningKey,
        config: &Config,
    ) -> crate::error::Result<String> {
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
        let name = server_profile_name(config);
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
            server_profile_capabilities(config),
            Some(records::ProfileOperator {
                did: did.to_string(),
                handle: account_handle.to_string(),
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
    async fn fetch_list(&mut self, reference: &str) -> crate::error::Result<(String, Vec<String>)> {
        let config = self.load_config().unwrap_or_default();
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        let list =
            crate::lists::resolve_list_reference(&self.http, &self.resolver, reference).await?;
        let (members, _) = self
            .cached_list_members(&appview, &list, config.list_cache_seconds, false)
            .await?;
        Ok((list, members))
    }

    /// The join-gate decision (M15, atpcraft's `isMember`): public AppView
    /// read, so it works logged out. Fail-closed — errors propagate and the
    /// handler reports them as the deny reason.
    async fn check_gate(
        &mut self,
        list: &str,
        subject: &str,
    ) -> crate::error::Result<crate::lists::GateOutcome> {
        let config = self.load_config().unwrap_or_default();
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        let gate = crate::lists::prepare_gate(&self.http, &self.resolver, list, subject).await?;
        let (members, fresh) = self
            .cached_list_members(&appview, &gate.list, config.list_cache_seconds, false)
            .await?;
        let allowed = if members.iter().any(|member| member == &gate.subject) {
            true
        } else if fresh {
            // atpcraft refreshes once on a fresh cache miss before rejecting.
            let (refreshed, _) = self
                .cached_list_members(&appview, &gate.list, config.list_cache_seconds, true)
                .await?;
            refreshed.iter().any(|member| member == &gate.subject)
        } else {
            false
        };
        Ok(crate::lists::GateOutcome {
            list: gate.list,
            subject: gate.subject,
            allowed,
        })
    }

    /// List members from the M52 cache. `fresh` is true only when the returned
    /// members came from a cache hit; a fetch always returns false. A failed
    /// fetch leaves the last successful cache untouched for the next attempt.
    async fn cached_list_members(
        &mut self,
        appview: &str,
        list: &str,
        ttl_seconds: u64,
        force: bool,
    ) -> crate::error::Result<(Vec<String>, bool)> {
        let now = Instant::now();
        let fresh = !force
            && self
                .list_cache
                .as_ref()
                .is_some_and(|cache| cache.is_fresh(appview, list, ttl_seconds, now));
        if fresh {
            let cache = self.list_cache.as_ref().expect("checked above");
            return Ok((cache.members.clone(), true));
        }
        let members = crate::lists::fetch_list_members(&self.http, appview, list).await?;
        self.list_cache = Some(ListCache {
            appview: appview.to_string(),
            list: list.to_string(),
            members: members.clone(),
            fetched_at: Instant::now(),
        });
        Ok((members, false))
    }

    /// Publish the current arm state to the runtime (M38). Called at startup
    /// and after a settings save, so a later login knows whether to ask. The
    /// sweep period rides along (M42) so the tick hook can throttle the online
    /// sweep; `0` disables it, and a positive value is floored at 60 seconds
    /// (atpcraft's list-cache floor).
    fn emit_join_gate_state(&self) {
        let config = self.load_config().unwrap_or_default();
        let sweep_seconds = if config.join_gate_sweep_seconds == 0 {
            0
        } else {
            config.join_gate_sweep_seconds.max(60)
        };
        self.emit(Event::JoinGateState {
            enabled: config.join_gate_enabled,
            list: config.join_gate_list,
            sweep_seconds,
        });
    }

    /// Enforce the gate for one joining player (M38). The runtime blocks the
    /// login for a bounded time; every failure is a refusal. A linked DID on
    /// the configured list is the only allow. An unlinked player is refused
    /// with a one-time browser link when the shared link token is configured
    /// (M40), so they can link without first being placed.
    async fn check_join_gate(&mut self, request_id: u64, player_uuid: &str, player_name: &str) {
        let config = self.load_config().unwrap_or_default();
        if !config.join_gate_enabled {
            self.emit(Event::JoinGateDecision {
                request_id,
                allowed: true,
                message: String::new(),
            });
            return;
        }
        if config.join_gate_list.trim().is_empty() {
            self.emit(Event::JoinGateDecision {
                request_id,
                allowed: false,
                message: JOIN_GATE_NO_LIST.into(),
            });
            return;
        }
        let verdict = tokio::time::timeout(
            JOIN_GATE_TIMEOUT,
            self.join_gate_verdict(&config, player_uuid, player_name),
        )
        .await;
        let (allowed, message) = match verdict {
            Ok(Ok(JoinGateVerdict::Allowed)) => (true, String::new()),
            Ok(Ok(JoinGateVerdict::NotMember)) => (false, JOIN_GATE_NOT_MEMBER.into()),
            Ok(Ok(JoinGateVerdict::Unlinked(message))) => (false, message),
            Ok(Err(_)) | Err(_) => (false, JOIN_GATE_UNAVAILABLE.into()),
        };
        self.emit(Event::JoinGateDecision {
            request_id,
            allowed,
            message,
        });
    }

    /// The gate verdict for one UUID. Any service or AppView error propagates
    /// so the caller refuses the join. The unlinked branch may create the
    /// browser link inside the same bounded gate read.
    async fn join_gate_verdict(
        &mut self,
        config: &Config,
        player_uuid: &str,
        player_name: &str,
    ) -> crate::error::Result<JoinGateVerdict> {
        if config.server_api_url.trim().is_empty() {
            return Err(AtmosphericError::Message(
                "server_api_url is not configured".into(),
            ));
        }
        let Some(link) =
            identity::fetch_link(&self.http, &config.server_api_url, player_uuid).await?
        else {
            return Ok(JoinGateVerdict::Unlinked(
                self.join_gate_unlinked_message(config, player_uuid, player_name)
                    .await,
            ));
        };
        let outcome = self.check_gate(&config.join_gate_list, &link.did).await?;
        Ok(if outcome.allowed {
            JoinGateVerdict::Allowed
        } else {
            JoinGateVerdict::NotMember
        })
    }

    /// The refusal for an unlinked player, including a freshly issued browser
    /// URL when the server has the shared link token. A failed link request is
    /// not fatal to the refusal; the player can still use an operator code.
    async fn join_gate_unlinked_message(
        &self,
        config: &Config,
        player_uuid: &str,
        player_name: &str,
    ) -> String {
        if config.server_link_token.trim().is_empty() {
            return JOIN_GATE_UNLINKED.into();
        }
        match identity::start_browser_link(
            &self.http,
            &config.server_api_url,
            &config.server_link_token,
            player_uuid,
            player_name,
            config.browser_link_ttl_seconds,
        )
        .await
        {
            Ok(link) => format!(
                "{JOIN_GATE_UNLINKED} Open {} within {} minutes to link, then reconnect.",
                link.url,
                config.browser_link_ttl_seconds.div_ceil(60)
            ),
            Err(_) => JOIN_GATE_UNLINKED.into(),
        }
    }

    /// Sweep the online players against the join gate (M42, atpcraft's
    /// `startWhitelistSweep` / `computeWhitelistKicks`). The runtime already
    /// throttled this to the configured period and supplied the online
    /// players. A failed list refresh — or a timeout — kicks nobody; the
    /// game side only disconnects the players reported here.
    async fn join_gate_sweep(&mut self, players: Vec<SweepPlayer>) {
        if players.is_empty() {
            return;
        }
        let config = self.load_config().unwrap_or_default();
        if !config.join_gate_enabled
            || config.join_gate_list.trim().is_empty()
            || config.server_api_url.trim().is_empty()
        {
            return;
        }
        match tokio::time::timeout(
            JOIN_GATE_SWEEP_TIMEOUT,
            self.join_gate_sweep_kicks(&config, &players),
        )
        .await
        {
            Ok(Ok(kicks)) if !kicks.is_empty() => {
                self.emit(Event::JoinGateKicks { players: kicks })
            }
            // A clean sweep, a failed refresh or a timeout: nothing to do.
            _ => {}
        }
    }

    /// The kick list for one sweep: the online players whose linked DID is not
    /// on a freshly read list. A failed list read propagates, so the caller
    /// kicks nobody. A per-player link lookup error — or an unlinked player —
    /// skips that player, matching atpcraft, which only kicks DIDs it can
    /// confirm are gone.
    async fn join_gate_sweep_kicks(
        &mut self,
        config: &Config,
        players: &[SweepPlayer],
    ) -> crate::error::Result<Vec<SweepPlayer>> {
        let (_, members) = self.fetch_list(&config.join_gate_list).await?;
        let members: HashSet<&str> = members.iter().map(String::as_str).collect();
        let mut kicks = Vec::new();
        for player in players {
            let Ok(Some(link)) =
                identity::fetch_link(&self.http, &config.server_api_url, &player.player_uuid).await
            else {
                continue;
            };
            if !members.contains(link.did.as_str()) {
                kicks.push(player.clone());
            }
        }
        Ok(kicks)
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

    /// Read an account's Bluesky profile (M24, atpcraft profile pronouns):
    /// the AppView's `getProfile` plus the direct `app.bsky.actor.profile`
    /// record from the account's own PDS, with atpcraft's precedence applied
    /// by [`profiles::fetch_profile`]. Both reads are unauthenticated, so this
    /// works logged out.
    async fn fetch_profile(
        &self,
        subject: &str,
    ) -> crate::error::Result<(String, String, profiles::ProfileDigest)> {
        let config = self.load_config().unwrap_or_default();
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        profiles::fetch_profile(&self.http, &self.resolver, &appview, subject).await
    }

    /// Read a player's `at.atpcraft.player.chat` records from their own PDS
    /// (M16, the read side of the M5 chat mirror): unauthenticated
    /// listRecords, ordered by `sentAt`, reduced to the total plus the newest
    /// few. Chat records are unsigned by design — no endorsement to verify.
    async fn fetch_chat(
        &self,
        subject: &str,
    ) -> crate::error::Result<(String, String, usize, Vec<chats::ChatDigest>)> {
        let (did, pds) = presence::resolve_player(&self.http, &self.resolver, subject).await?;
        let records =
            presence::list_records(&self.http, &pds, &did, records::CHAT_COLLECTION).await?;
        let (total, items) = chats::history(&did, &records, chats::HISTORY_LIMIT);
        Ok((did, pds, total, items))
    }

    /// Read generic records from any collection in a subject's own PDS (M44):
    /// unauthenticated listRecords, reduced to the total plus the newest few
    /// records as compact JSON strings. The records are arbitrary lexicons, so
    /// nothing is parsed or verified.
    async fn fetch_records(
        &self,
        subject: &str,
        collection: &str,
    ) -> crate::error::Result<(String, String, String, usize, Vec<String>)> {
        let (did, pds) = presence::resolve_player(&self.http, &self.resolver, subject).await?;
        let records = presence::list_records(&self.http, &pds, &did, collection).await?;
        let total = records.len();
        // listRecords serves a collection newest-first; take the newest few and
        // reverse them into display order (oldest first).
        let items = records
            .iter()
            .take(RECORDS_HISTORY_LIMIT)
            .rev()
            .map(|(_, value)| value.to_string())
            .collect();
        Ok((did, pds, collection.to_string(), total, items))
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

    /// Read another account's `at.atpcraft.server.profile` record from their
    /// own PDS, verify its signing key against the trusted set (the mod's own
    /// signing key plus `trusted_server_keys`), and return the HUD digest
    /// (M19, the read side of M10). The listRecords call is unauthenticated,
    /// so this works logged out.
    async fn fetch_server_profile(
        &self,
        subject: &str,
    ) -> crate::error::Result<(String, String, Option<servers::ServerDigest>)> {
        let config = self.load_config().unwrap_or_default();
        let (did, pds) = servers::resolve_server(&self.http, &self.resolver, subject).await?;
        let records =
            servers::list_records(&self.http, &pds, &did, records::SERVER_PROFILE_COLLECTION)
                .await?;
        let keys = self.verification_keys(&config);
        let Some((rkey, value)) = servers::latest(&records) else {
            return Ok((did, pds, None));
        };
        let digest = servers::digest(&did, &rkey, &value, &keys)?;
        Ok((did, pds, Some(digest)))
    }

    /// Read one of an account's public social graphs (M20): follows,
    /// followers, blocks or mutes via the AppView. The reads are
    /// unauthenticated, so this works logged out.
    async fn fetch_graph(
        &self,
        kind: &str,
        subject: &str,
    ) -> crate::error::Result<(String, String, String, Vec<graph::GraphMember>)> {
        let kind = graph::GraphKind::from_key(kind).ok_or_else(|| {
            AtmosphericError::Message(format!(
                "unknown graph kind {kind:?} (want follows, followers, blocks or mutes)"
            ))
        })?;
        let config = self.load_config().unwrap_or_default();
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        let actor = graph::resolve_subject(&self.http, &self.resolver, subject).await?;
        let members = graph::fetch_graph(&self.http, &appview, kind, &actor).await?;
        Ok((kind.key().to_string(), actor, appview, members))
    }

    /// Write one relationship (M23): follow/block create a record in your
    /// repo, unfollow/unblock find and delete the matching record, and
    /// mute/unmute call the AppView's private mute procedures. The subject is
    /// resolved to a DID before anything is written, and self-actions are
    /// refused before any network call.
    async fn graph_write(
        &mut self,
        action: &str,
        subject: &str,
    ) -> crate::error::Result<(String, String)> {
        let action = graph::GraphAction::from_key(action).ok_or_else(|| {
            AtmosphericError::Message(format!(
                "unknown graph action {action:?} (want follow, unfollow, block, unblock, mute or unmute)"
            ))
        })?;
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let target = graph::resolve_subject(&self.http, &self.resolver, subject).await?;
        if target == *did {
            return Err(AtmosphericError::Message(
                "you cannot follow, block or mute your own account".into(),
            ));
        }
        match action {
            graph::GraphAction::Follow => {
                let rkey = self.tid.next_tid();
                actions::create_record_json(
                    agent,
                    did,
                    graph::FOLLOW_COLLECTION,
                    Some(&rkey),
                    graph::follow_record(&target),
                )
                .await?;
            }
            graph::GraphAction::Block => {
                let rkey = self.tid.next_tid();
                actions::create_record_json(
                    agent,
                    did,
                    graph::BLOCK_COLLECTION,
                    Some(&rkey),
                    graph::block_record(&target),
                )
                .await?;
            }
            graph::GraphAction::Unfollow | graph::GraphAction::Unblock => {
                let collection = action.collection().expect("removals have a collection");
                let config = self.load_config().unwrap_or_default();
                let pds = if config.pds_url.is_empty() {
                    "https://bsky.social".to_string()
                } else {
                    config.pds_url
                };
                // Public repos serve listRecords unauthenticated, so the
                // worker reads its own repo the same way the M13/M14 reads do.
                let records = presence::list_records(&self.http, &pds, did, collection).await?;
                let rkeys = graph::rkeys_for_subject(&records, &target);
                if rkeys.is_empty() {
                    let verb = if action == graph::GraphAction::Unfollow {
                        "following"
                    } else {
                        "blocking"
                    };
                    return Err(AtmosphericError::Message(format!(
                        "you are not {verb} {target}"
                    )));
                }
                for rkey in rkeys {
                    actions::delete_record(agent, did, collection, &rkey).await?;
                }
            }
            graph::GraphAction::Mute => actions::mute_actor(agent, &target).await?,
            graph::GraphAction::Unmute => actions::unmute_actor(agent, &target).await?,
        }
        Ok((action.key().to_string(), target))
    }

    /// Reply to a post (M21): fetch the target's strong ref + thread root
    /// from the author's PDS, then publish an `app.bsky.feed.post` with
    /// `reply: { root, parent }` to your own repo.
    async fn reply(&self, uri: &str, text: &str) -> crate::error::Result<String> {
        if text.trim().is_empty() {
            return Err(AtmosphericError::Message("the reply text is empty".into()));
        }
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let target = interactions::resolve_post(&self.http, &self.resolver, uri).await?;
        let (parent, value) = interactions::fetch_ref(&self.http, &target.pds, &target.uri).await?;
        let targets = interactions::reply_targets(parent, &value);
        let record = interactions::reply_record(text, &targets);
        actions::create_record_json(agent, did, interactions::POST_COLLECTION, None, record).await
    }

    /// Like a post (M21): fetch its strong ref and publish an
    /// `app.bsky.feed.like` record to your own repo.
    async fn like(&self, uri: &str) -> crate::error::Result<String> {
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let target = interactions::resolve_post(&self.http, &self.resolver, uri).await?;
        let (parent, _) = interactions::fetch_ref(&self.http, &target.pds, &target.uri).await?;
        actions::create_record_json(
            agent,
            did,
            interactions::LIKE_COLLECTION,
            None,
            interactions::like_record(&parent),
        )
        .await
    }

    /// Repost a post (M21): fetch its strong ref and publish an
    /// `app.bsky.feed.repost` record to your own repo.
    async fn repost(&self, uri: &str) -> crate::error::Result<String> {
        let (Some(agent), Some(did)) = (&self.agent, &self.did) else {
            return Err(AtmosphericError::NotLoggedIn);
        };
        let target = interactions::resolve_post(&self.http, &self.resolver, uri).await?;
        let (parent, _) = interactions::fetch_ref(&self.http, &target.pds, &target.uri).await?;
        actions::create_record_json(
            agent,
            did,
            interactions::REPOST_COLLECTION,
            None,
            interactions::repost_record(&parent),
        )
        .await
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

    /// Claim a server identity pairing code as the logged-in player (M36).
    /// Nothing is sent unless the explicit command is used, and the request
    /// carries no session token or game data.
    async fn claim_server_link(&self, code: &str) {
        let Some(did) = self.did.as_deref() else {
            self.fail("not logged in: /atmospheric link needs a player account");
            return;
        };
        let config = match self.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.fail(format!("could not read the settings file: {error}"));
                return;
            }
        };
        if config.server_api_url.trim().is_empty() {
            self.fail(
                "server_api_url is not configured: set the M35 identity service URL in config.json",
            );
            return;
        }
        let handle = self
            .stored_session
            .as_ref()
            .map(|session| session.handle.trim())
            .filter(|handle| !handle.is_empty());
        match identity::claim(&self.http, &config.server_api_url, code, did, handle).await {
            Ok(link) => self.emit(Event::Status(format!(
                "atmospheric: linked {} ({}) to {}",
                link.player_name, link.player_uuid, link.did
            ))),
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// Start M39 browser linking for an online player. The worker asks the
    /// identity service for a one-time URL using the server-only link token.
    /// The player's UUID was supplied by the game hook; the browser never
    /// chooses it, so an OAuth login cannot claim another player's UUID.
    async fn start_browser_link(&self, player_uuid: &str, player_name: &str) {
        let config = match self.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.fail(format!("could not read the settings file: {error}"));
                return;
            }
        };
        if config.server_api_url.trim().is_empty() {
            self.fail(
                "server_api_url is not configured: set the M35 identity service URL in config.json",
            );
            return;
        }
        if config.server_link_token.trim().is_empty() {
            self.fail(
                "server_link_token is not configured: copy the identity service's ATMOSPHERIC_SERVER_LINK_TOKEN into config.json",
            );
            return;
        }
        match identity::start_browser_link(
            &self.http,
            &config.server_api_url,
            &config.server_link_token,
            player_uuid,
            player_name,
            config.browser_link_ttl_seconds,
        )
        .await
        {
            Ok(link) => self.emit(Event::BrowserLink {
                player_uuid: player_uuid.to_owned(),
                url: link.url,
                expires_at: link.expires_at,
            }),
            Err(error) => self.fail(error.to_string()),
        }
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
                self.colibri_profile_checked = false;
                self.colibri_player_membership_checked = false;
                self.check_colibri_player_membership().await;
                self.check_colibri_profile().await;
            }
            Err(error) => self.emit(Event::Session(SessionState::Failed {
                message: AtmosphericError::protocol(error).to_string(),
            })),
        }
    }

    /// Start a localhost OAuth login for the player (M8).
    fn start_login_oauth(&mut self, handle: String) {
        self.start_oauth(handle, OAuthTarget::Player);
    }

    /// Start a localhost OAuth login for the separate server account (M25).
    fn start_server_login_oauth(&mut self, handle: String) {
        self.start_oauth(handle, OAuthTarget::Server);
    }

    /// Start a localhost OAuth login (M8/M25). The browser flow runs on a
    /// spawned task so the command loop stays responsive (Shutdown works while
    /// the browser is open); its result arrives on the oauth channel tagged
    /// with the target session.
    fn start_oauth(&mut self, handle: String, target: OAuthTarget) {
        match target {
            OAuthTarget::Player => self.emit(Event::Session(SessionState::LoggingIn)),
            OAuthTarget::Server => {
                self.server_handle = Some(handle.clone());
                self.emit_server_account(SessionState::LoggingIn);
            }
        }
        // The client_id comes from the settings file when set (hosted client
        // metadata URL); otherwise the loopback default. The scope is the
        // configured base (or the mod's default) plus the grants the features
        // need; the server account also asks for its descriptor grant and the
        // Colibri stream grant, because otherwise a narrow custom scope or a
        // bridged server fails its writes / service-auth mint.
        let config = self.load_config().ok();
        let scope = config
            .as_ref()
            .map(|config| match target {
                OAuthTarget::Server => oauth::scope_for_server(
                    &config.oauth_scope,
                    config.colibri_bridge_enabled,
                    &config.colibri_appview_did,
                ),
                OAuthTarget::Player => oauth::scope_for(
                    &config.oauth_scope,
                    config.colibri_bridge_enabled,
                    &config.colibri_appview_did,
                ),
            })
            .unwrap_or_else(|| oauth::DEFAULT_SCOPE.into());
        let client_id = config
            .as_ref()
            .map(|config| config.oauth_client_id(&scope))
            .unwrap_or_else(|| oauth::client_id(&scope));
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
                scope: &scope,
                redirect_timeout: oauth::REDIRECT_WAIT,
                launch_browser,
                events: &events,
            })
            .await;
            let _ = oauth_tx.send((target, result));
        });
    }

    /// Apply a finished OAuth login to the session it belongs to.
    async fn finish_login_oauth(&mut self, target: OAuthTarget, login: oauth::OAuthLogin) {
        match target {
            OAuthTarget::Player => self.finish_player_oauth(login).await,
            OAuthTarget::Server => self.finish_server_oauth(login).await,
        }
    }

    /// Apply a finished player OAuth login: build the DPoP-signed agent,
    /// validate the session, persist the refresh token and DPoP key, report
    /// LoggedIn.
    async fn finish_player_oauth(&mut self, login: oauth::OAuthLogin) {
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
                self.colibri_profile_checked = false;
                self.colibri_player_membership_checked = false;
                self.check_colibri_player_membership().await;
                self.check_colibri_profile().await;
            }
            Err(error) => self.emit(Event::Session(SessionState::Failed {
                message: error.to_string(),
            })),
        }
    }

    /// Apply a finished server-account OAuth login (M25). The server session is
    /// independent of the player session and always persisted: its purpose is
    /// to act when no player is logged in.
    async fn finish_server_oauth(&mut self, login: oauth::OAuthLogin) {
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
                self.server_agent = Some(agent);
                self.server_did = Some(did.clone());
                self.server_handle = Some(handle.clone());
                self.server_dpop = dpop;
                if let Ok(config) = self.load_config() {
                    self.server_token_refresh_secs =
                        config.server_account_token_refresh_seconds.max(1);
                }
                let stored = StoredSession {
                    pds_url,
                    did: did.clone(),
                    handle,
                    refresh_jwt: tokens.refresh_token.clone().unwrap_or_default(),
                    oauth: Some(session::StoredOAuth {
                        dpop_jwk: key.to_jwk_json(),
                        token_endpoint,
                        issuer,
                        client_id,
                        scope,
                    }),
                };
                self.server_stored_session = Some(stored.clone());
                if let Err(error) = self.save_server_session(&stored) {
                    self.fail(format!(
                        "could not persist the server-account session file: {error}"
                    ));
                }
                self.emit_server_account(SessionState::LoggedIn { did });
                self.maybe_start_colibri().await;
                self.auto_publish_server_profile().await;
            }
            Err(error) => self.emit_server_account(SessionState::Failed {
                message: error.to_string(),
            }),
        }
    }

    /// Report a server-account event with the handle and DID the worker knows.
    fn emit_server_account(&self, state: SessionState) {
        self.emit(Event::ServerAccount {
            state,
            handle: self.server_handle.clone(),
            did: self.server_did.clone(),
        });
    }

    /// Report the current server-account state on demand (M25).
    fn emit_server_account_status(&self) {
        let state = match &self.server_did {
            Some(did) => SessionState::LoggedIn { did: did.clone() },
            None => SessionState::LoggedOut,
        };
        self.emit_server_account(state);
    }

    /// Drop the server-account session and delete its persisted refresh token.
    fn logout_server_account(&mut self) {
        self.stop_colibri(true);
        self.server_agent = None;
        self.server_did = None;
        self.server_handle = None;
        self.server_stored_session = None;
        self.server_dpop = oauth::new_shared_dpop();
        let path = self.storage_dir.join(SERVER_SESSION_FILE);
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            self.fail(format!(
                "could not delete the server-account session file: {error}"
            ));
            return;
        }
        self.emit_server_account(SessionState::LoggedOut);
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

    /// Refresh the server account's DPoP-bound access token when it is close
    /// to expiry (M25), independently of the player session.
    async fn ensure_server_oauth_fresh(&mut self) {
        let needs_refresh = self
            .server_dpop
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|state| state.expires_within(60));
        if !needs_refresh {
            return;
        }
        let (Some(state), Some(mut stored)) = (
            self.server_dpop.lock().unwrap().clone(),
            self.server_stored_session.clone(),
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
                *self.server_dpop.lock().unwrap() =
                    Some(DpopState::from_tokens(state.key, &tokens));
                stored.refresh_jwt = tokens.refresh_token.clone().unwrap_or(stored.refresh_jwt);
                self.server_stored_session = Some(stored.clone());
                if let Err(error) = self.save_server_session(&stored) {
                    self.fail(format!(
                        "could not persist the server-account session file: {error}"
                    ));
                }
            }
            Err(error) => self.emit_server_account(SessionState::Failed {
                message: format!("could not refresh the server-account session: {error}"),
            }),
        }
    }

    /// Proactively refresh the server account's OAuth token (M50). The
    /// existing expiry check skips the network while the token is healthy.
    async fn refresh_server_token(&mut self) {
        if self.server_agent.is_none() || self.server_did.is_none() {
            return;
        }
        self.ensure_server_oauth_fresh().await;
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

    /// Resume the persisted server-account session (M25). The server session
    /// is OAuth-only, so it resumes through the authorization server's DPoP
    /// refresh grant, never the player session file.
    async fn try_resume_server(&mut self) {
        let Some(stored) = self.server_stored_session.clone() else {
            return;
        };
        let Some(oauth) = &stored.oauth else {
            return;
        };
        match session::resume_oauth(&self.http, &stored, oauth).await {
            Ok((agent, dpop, rotated)) => {
                let did = rotated.did.clone();
                self.server_agent = Some(agent);
                self.server_did = Some(did.clone());
                self.server_handle = Some(rotated.handle.clone());
                self.server_dpop = dpop;
                self.server_stored_session = Some(rotated.clone());
                if let Ok(config) = self.load_config() {
                    self.server_token_refresh_secs =
                        config.server_account_token_refresh_seconds.max(1);
                }
                if let Err(error) = self.save_server_session(&rotated) {
                    self.fail(format!(
                        "could not persist the server-account session file: {error}"
                    ));
                }
                self.emit_server_account(SessionState::LoggedIn { did });
                self.maybe_start_colibri().await;
                self.auto_publish_server_profile().await;
            }
            Err(error) => {
                self.server_stored_session = None;
                self.emit_server_account(SessionState::Failed {
                    message: format!(
                        "could not resume the server-account session ({error}) — log in again with /atmospheric server-account login <handle>"
                    ),
                });
            }
        }
    }

    /// Start the bridge when the settings file asks for it and the server
    /// account is available. Called after a login or a resumed session.
    async fn maybe_start_colibri(&mut self) {
        if self.colibri_channel.is_some() {
            return;
        }
        let Ok(config) = self.load_config() else {
            return;
        };
        if !config.colibri_bridge_enabled || config.colibri_channel_uri.trim().is_empty() {
            return;
        }
        self.start_colibri().await;
    }

    /// Arm the event stream. The server account mints the service-auth token;
    /// player messages are written separately by [`Self::send_colibri`].
    async fn start_colibri(&mut self) {
        let config = match self.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.fail(format!("could not read the settings file: {error}"));
                return;
            }
        };
        let Some(channel) = ChannelRef::parse(&config.colibri_channel_uri) else {
            self.fail(format!(
                "Colibri channel is neither an AT-URI nor a Colibri link: '{}'",
                config.colibri_channel_uri
            ));
            return;
        };
        let Some(server_did) = self.server_did.clone() else {
            self.fail(
                "log in the server account first: /atmospheric server-account login <handle>",
            );
            return;
        };
        self.ensure_server_oauth_fresh().await;
        let Some(agent) = self.server_agent.as_ref() else {
            self.fail("the server account is not logged in");
            return;
        };
        let membership_rkey = self.tid.next_tid();
        let created = match colibri::ensure_membership(
            agent,
            &server_did,
            &channel,
            &membership_rkey,
        )
        .await
        {
            Ok(created) => created,
            Err(error) => {
                self.fail(format!("could not declare Colibri membership: {error}"));
                return;
            }
        };
        if created
            && colibri::requires_approval(&self.http, &self.resolver, &channel.community_did).await
                == Some(true)
        {
            self.emit(Event::Status(
                "Colibri membership is pending; a moderator must approve it before messages appear."
                    .into(),
            ));
        }
        let aud = colibri::appview_service_ref(&config.colibri_appview_did);
        let token = match colibri::service_auth(agent, &aud, colibri::SUBSCRIBE_EVENTS_LXM).await {
            Ok(token) => token,
            Err(error) => {
                self.fail(format!("could not mint the Colibri stream token: {error}"));
                return;
            }
        };
        self.stop_colibri(false);
        self.colibri_channel = Some(channel);
        self.colibri_connected = false;
        self.colibri_sent.clear();
        self.colibri_started_at = Some(Instant::now());
        self.spawn_colibri_stream(config.colibri_appview_url.clone(), token);
        self.emit_colibri(None);
        self.check_colibri_player_membership().await;
        self.check_colibri_profile().await;
    }

    /// Replace the stream task while keeping the bridge state. Used by both
    /// token rotation and the explicit reconnect command.
    fn spawn_colibri_stream(&mut self, appview: String, token: String) {
        if let Some(task) = self.colibri_task.take() {
            task.abort();
        }
        let tx = self.colibri_tx.clone();
        self.colibri_task = Some(tokio::spawn(async move {
            colibri::run_stream(appview, token, tx).await;
        }));
    }

    /// Reconnect only the event stream (M31), mirroring atpcraft's
    /// `/atpcraft colibri reconnect`: keep the channel, echo guard, presence
    /// generation and profile-check state, re-mint the service token as the
    /// server account, and open a new socket. Membership is not repaired and
    /// the profile is not checked again.
    async fn reconnect_colibri(&mut self) {
        if self.colibri_channel.is_none() {
            self.fail("the Colibri bridge is not running");
            return;
        }
        if self.server_agent.is_none() || self.server_did.is_none() {
            self.fail("the server account is not logged in");
            return;
        }
        let config = match self.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.fail(format!("could not read the settings file: {error}"));
                return;
            }
        };
        self.ensure_server_oauth_fresh().await;
        let aud = colibri::appview_service_ref(&config.colibri_appview_did);
        let token = {
            let Some(agent) = self.server_agent.as_ref() else {
                self.fail("the server account is not logged in");
                return;
            };
            colibri::service_auth(agent, &aud, colibri::SUBSCRIBE_EVENTS_LXM).await
        };
        let token = match token {
            Ok(token) => token,
            Err(error) => {
                self.fail(format!("could not mint the Colibri stream token: {error}"));
                return;
            }
        };
        self.colibri_connected = false;
        self.spawn_colibri_stream(config.colibri_appview_url.clone(), token);
        self.emit_colibri(None);
    }

    /// Repair the local player's own Colibri community membership once per
    /// bridge session (M32). atpcraft writes this record when a player
    /// authorizes, so the AppView can index that player's messages. A failure
    /// is logged-only upstream and must never disturb the bridge, so this stays
    /// quiet and leaves the guard unset for a later login or rotation.
    async fn check_colibri_player_membership(&mut self) {
        if self.colibri_player_membership_checked || self.colibri_channel.is_none() {
            return;
        }
        let Some(channel) = self.colibri_channel.clone() else {
            return;
        };
        let Some(did) = self.did.clone() else {
            return;
        };
        self.ensure_oauth_fresh().await;
        let rkey = self.tid.next_tid();
        let result = {
            let Some(agent) = self.agent.as_ref() else {
                return;
            };
            colibri::ensure_membership(agent, &did, &channel, &rkey).await
        };
        if let Ok(created) = result {
            self.colibri_player_membership_checked = true;
            if created
                && colibri::requires_approval(&self.http, &self.resolver, &channel.community_did)
                    .await
                    == Some(true)
            {
                self.emit(Event::ColibriMembershipPending {
                    message: colibri::MEMBERSHIP_PENDING_MESSAGE.into(),
                });
            }
        }
    }

    /// Check the local player's Colibri actor profile once per bridge session
    /// (M30). atpcraft prompts every linked player on join; a client mod only
    /// knows the local player's session, so this checks that account. A
    /// missing record emits the setup prompt; a failed read stays quiet.
    async fn check_colibri_profile(&mut self) {
        if self.colibri_profile_checked || self.colibri_channel.is_none() {
            return;
        }
        let Some(did) = self.did.clone() else {
            return;
        };
        self.ensure_oauth_fresh().await;
        let exists = match self.agent.as_ref() {
            Some(agent) => colibri::profile_exists(agent, &did).await,
            None => return,
        };
        self.colibri_profile_checked = true;
        if matches!(exists, Ok(false)) {
            self.emit(Event::ColibriProfileMissing {
                url: colibri::PROFILE_URL.into(),
            });
        }
    }

    /// Check a linked joining player's Colibri profile (M37). atpcraft checks
    /// every linked player on join; the UUID comes from the server hook and the
    /// DID from the M35 map. Every failure is quiet, matching the M30 rule for
    /// an unavailable profile read.
    async fn check_remote_colibri_profile(&self, player_uuid: &str, player_name: &str) {
        if self.colibri_channel.is_none() {
            return;
        }
        let Ok(config) = self.load_config() else {
            return;
        };
        if config.server_api_url.trim().is_empty() {
            return;
        }
        let Ok(Some(link)) =
            identity::fetch_link(&self.http, &config.server_api_url, player_uuid).await
        else {
            return;
        };
        if self.did.as_deref() == Some(link.did.as_str()) {
            return; // M30 already checks the local player.
        }
        if matches!(
            colibri::remote_profile_exists(&self.http, &self.resolver, &link.did).await,
            Ok(false)
        ) {
            self.emit(Event::ColibriRemoteProfileMissing {
                player_uuid: player_uuid.to_string(),
                player_name: player_name.to_string(),
                url: colibri::PROFILE_URL.into(),
            });
        }
    }

    /// Apply a linked joining player's Bluesky display name and pronouns as
    /// their in-game name (M45). atpcraft shows display names and pronouns for
    /// linked players; the worker resolves the DID through the M35 map, reads
    /// the profile with M24's two-source precedence, and emits the composed
    /// nickname. Every failure is quiet: an unlinked player, an unreachable
    /// service or a profile with no usable name changes nothing.
    async fn check_player_profile(&self, player_uuid: &str, player_name: &str) {
        let Ok(config) = self.load_config() else {
            return;
        };
        if config.server_api_url.trim().is_empty() {
            return;
        }
        let Ok(Some(link)) =
            identity::fetch_link(&self.http, &config.server_api_url, player_uuid).await
        else {
            return;
        };
        let appview = if config.appview_url.is_empty() {
            crate::lists::DEFAULT_APPVIEW.to_string()
        } else {
            config.appview_url.clone()
        };
        let Ok((_, _, profile)) =
            profiles::fetch_profile(&self.http, &self.resolver, &appview, &link.did).await
        else {
            return;
        };
        if let Some(nickname) = profiles::nickname_for(player_name, &profile) {
            self.emit(Event::PlayerProfile {
                player_uuid: player_uuid.to_string(),
                player_name: player_name.to_string(),
                nickname,
            });
        }
    }

    /// Disarm the bridge and, unless called from a replace, report it.
    fn stop_colibri(&mut self, announce: bool) {
        if let Some(task) = self.colibri_task.take() {
            task.abort();
        }
        self.colibri_channel = None;
        self.colibri_connected = false;
        self.colibri_sent.clear();
        self.colibri_last_join = None;
        self.colibri_last_leave = None;
        self.colibri_last_blank = None;
        self.colibri_started_at = None;
        if announce {
            self.colibri_profile_checked = false;
            self.colibri_player_membership_checked = false;
            self.emit_colibri(None);
        }
    }

    fn emit_colibri(&self, error: Option<String>) {
        self.emit(Event::Colibri {
            active: self.colibri_channel.is_some(),
            connected: self.colibri_connected,
            channel: self
                .colibri_channel
                .as_ref()
                .map(|channel| channel.channel_uri.clone())
                .unwrap_or_default(),
            community: self
                .colibri_channel
                .as_ref()
                .map(ChannelRef::community_uri)
                .unwrap_or_default(),
            error,
        });
    }

    /// Handle one update from the socket loop. Only messages in the bridged
    /// channel are surfaced, and the worker's own writes are filtered out.
    async fn handle_colibri_update(&mut self, update: StreamEvent) {
        match update {
            StreamEvent::Connected => {
                self.colibri_connected = true;
                self.emit_colibri(None);
            }
            StreamEvent::Disconnected(error) => {
                self.colibri_connected = false;
                self.emit_colibri(Some(error));
            }
            StreamEvent::Rotate => self.start_colibri().await,
            StreamEvent::FallbackDue { kind, text, at } => {
                if self.colibri_presence_needed(&kind, at) {
                    self.send_colibri_notice(&text).await;
                }
            }
            StreamEvent::Message(message) => {
                let Some(channel) = &self.colibri_channel else {
                    return;
                };
                if message.channel != channel.channel_uri
                    || self.colibri_sent.iter().any(|uri| uri == &message.uri)
                {
                    return;
                }
                self.emit(Event::ColibriMessage {
                    handle: message.author_handle,
                    text: message.text,
                });
            }
        }
    }

    /// Publish one chat line as the player account. The explicit command
    /// reports failures; the chat hook mirror stays quiet when the bridge is
    /// off so ordinary chat never produces an error.
    async fn send_colibri(&mut self, text: &str, automatic: bool) {
        let Some(channel) = self.colibri_channel.clone() else {
            if !automatic {
                self.fail("the Colibri bridge is not running");
            }
            return;
        };
        let text = text.trim();
        if text.is_empty() {
            if !automatic {
                self.fail("the Colibri message is empty");
            }
            return;
        }
        if !self.allow_colibri_send() {
            if !automatic {
                self.fail("Colibri send rate limit reached (20 messages per minute)");
            }
            return;
        }
        self.ensure_oauth_fresh().await;
        let rkey = self.tid.next_tid();
        let record = colibri::message_record(text, &channel.channel_uri, &colibri::now());
        let result = match (&self.agent, &self.did) {
            (Some(agent), Some(did)) => {
                actions::create_record_json(
                    agent,
                    did,
                    colibri::MESSAGE_COLLECTION,
                    Some(&rkey),
                    record,
                )
                .await
            }
            _ => Err(AtmosphericError::NotLoggedIn),
        };
        match result {
            Ok(uri) => {
                self.remember_colibri_uri(uri.clone());
                if !automatic {
                    self.emit(Event::Posted { uri });
                }
            }
            Err(error) if !automatic => self.fail(error.to_string()),
            Err(_) => {}
        }
    }

    /// Count one player-authored send, resetting the window when the account
    /// changes. Server notices and stream messages do not pass through here.
    fn allow_colibri_send(&mut self) -> bool {
        let did = self.did.clone().unwrap_or_default();
        if self.colibri_rate_did.as_deref() != Some(did.as_str()) {
            self.colibri_rate_did = Some(did);
            self.colibri_send_times.clear();
        }
        colibri_send_allowed(&mut self.colibri_send_times, Instant::now())
    }

    /// Record a join/leave (or blank/hidden) line for the M29 fallback. This
    /// mirrors atpcraft's `ColibriPresenceFallback`: only the latest time per
    /// kind matters, and a blank line counts for both join and leave.
    fn mark_colibri_handled(&mut self, kind: Option<&str>) {
        if self.colibri_channel.is_none() {
            return;
        }
        let Ok(config) = self.load_config() else {
            return;
        };
        if !config.colibri_server_events_enabled {
            return;
        }
        let now = Instant::now();
        match kind {
            Some("join") => self.colibri_last_join = Some(now),
            Some("leave") => self.colibri_last_leave = Some(now),
            Some(_) => {}
            None => self.colibri_last_blank = Some(now),
        }
    }

    /// Schedule the presence fallback for a connection event (M29). The timer
    /// runs off-thread and comes back through the Colibri event channel, so the
    /// command loop stays responsive.
    fn schedule_colibri_fallback(&self, kind: &str, text: String) {
        if self.colibri_channel.is_none() || text.trim().is_empty() {
            return;
        }
        let Ok(config) = self.load_config() else {
            return;
        };
        if !config.colibri_server_events_enabled || !config.colibri_server_events_fallback_enabled {
            return;
        }
        let at = Instant::now();
        let tx = self.colibri_tx.clone();
        let kind = kind.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(COLIBRI_PRESENCE_WAIT).await;
            let _ = tx.send(StreamEvent::FallbackDue { kind, text, at });
        });
    }

    /// Whether a due fallback still needs to write. The event must belong to
    /// the current bridge generation, both switches must still be on, and no
    /// join/leave or blank line may have arrived within the window.
    fn colibri_presence_needed(&self, kind: &str, at: Instant) -> bool {
        if self.colibri_channel.is_none() {
            return false;
        }
        if self.colibri_started_at.is_none_or(|started| at < started) {
            return false;
        }
        let Ok(config) = self.load_config() else {
            return false;
        };
        if !config.colibri_server_events_enabled || !config.colibri_server_events_fallback_enabled {
            return false;
        }
        presence_notice_needed(
            self.colibri_last_join,
            self.colibri_last_leave,
            self.colibri_last_blank,
            kind,
            at,
        )
    }

    /// Relay one classified server broadcast as the server account (M28).
    /// This is automatic: a missing bridge, disabled notices or a failed write
    /// stay silent rather than interrupting the game every time someone joins.
    async fn send_colibri_notice(&mut self, text: &str) {
        let Some(channel) = self.colibri_channel.clone() else {
            return;
        };
        let Ok(config) = self.load_config() else {
            return;
        };
        if !config.colibri_server_events_enabled {
            return;
        }
        let text: String = text
            .chars()
            .filter(|c| !c.is_control() && *c != '§')
            .take(colibri::MAX_TEXT_LENGTH)
            .collect();
        if text.trim().is_empty() {
            return;
        }
        self.ensure_server_oauth_fresh().await;
        let rkey = self.tid.next_tid();
        let record = colibri::message_record(&text, &channel.channel_uri, &colibri::now());
        let result = match (&self.server_agent, &self.server_did) {
            (Some(agent), Some(did)) => {
                actions::create_record_json(
                    agent,
                    did,
                    colibri::MESSAGE_COLLECTION,
                    Some(&rkey),
                    record,
                )
                .await
            }
            _ => Err(AtmosphericError::NotLoggedIn),
        };
        if let Ok(uri) = result {
            self.remember_colibri_uri(uri);
        }
    }

    fn remember_colibri_uri(&mut self, uri: String) {
        self.colibri_sent.push_back(uri);
        while self.colibri_sent.len() > 256 {
            self.colibri_sent.pop_front();
        }
    }

    /// Persist a session's refresh token (worker owns disk I/O). For OAuth
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

    /// Persist the server account's session separately from the player's
    /// (M25). It holds the DPoP private key, so it is owner-only.
    fn save_server_session(&self, stored: &StoredSession) -> crate::error::Result<()> {
        std::fs::create_dir_all(&self.storage_dir)?;
        write_private(
            &self.storage_dir.join(SERVER_SESSION_FILE),
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
            list_cache_seconds: 300,
            server_account_token_refresh_seconds: 60,
            post_death_messages: false,
            post_chat_messages: false,
            chat_server_id: "test-server".into(),
            persist_session: true,
            oauth_client_id: String::new(),
            oauth_scope: String::new(),
            skin_path: String::new(),
            cape_path: String::new(),
            skin_model: "default".into(),
            client_name: "atmospheric-horizon".into(),
            server_name: String::new(),
            server_description: String::new(),
            server_address: String::new(),
            server_website: String::new(),
            server_contact: String::new(),
            server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
            game_version: "26.3".into(),
            appview_url: "https://public.api.bsky.app".into(),
            server_api_url: String::new(),
            server_link_token: String::new(),
            browser_link_ttl_seconds: 600,
            join_gate_enabled: false,
            join_gate_list: String::new(),
            join_gate_sweep_seconds: 60,
            trusted_server_keys: Vec::new(),
            colibri_bridge_enabled: false,
            colibri_channel_uri: String::new(),
            colibri_appview_url: "https://api.colibri.social".into(),
            colibri_appview_did: "did:web:api.colibri.social".into(),
            colibri_server_events_enabled: true,
            colibri_server_events_fallback_enabled: true,
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

        /// The captured requests minus the worker's own background
        /// notification poll. After a login the poll timer fires immediately
        /// (tokio's first `interval` tick) and then on its configured period,
        /// so a `listNotifications` request can land at any moment; the
        /// "no HTTP call" assertions must not be raced by it.
        fn non_poll_requests(&self) -> Vec<Captured> {
            self.requests()
                .into_iter()
                .filter(|r| {
                    !r.path
                        .starts_with("/xrpc/app.bsky.notification.listNotifications")
                })
                .collect()
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
        } else if path == "/v1/pairings/claim" {
            (
                "200 OK",
                r#"{"player_uuid":"00000000-0000-4000-8000-000000000001","player_name":"Alex","did":"did:plc:stubuser","handle":"stub.test","linked_at":1760000000}"#.to_string(),
            )
        } else if path == "/v1/browser/pairings" {
            (
                "201 Created",
                r#"{"url":"https://links.test/link/browser-token","expires_at":1760000600}"#
                    .to_string(),
            )
        } else if path == "/v1/links/00000000-0000-4000-8000-0000000000a1" {
            (
                "200 OK",
                r#"{"player_uuid":"00000000-0000-4000-8000-0000000000a1","player_name":"Remote","did":"did:plc:remote","handle":null,"linked_at":1760000000}"#.to_string(),
            )
        } else if path == "/v1/links/00000000-0000-4000-8000-0000000000a2" {
            (
                "404 Not Found",
                r#"{"error":"no link for player"}"#.to_string(),
            )
        } else if path == "/v1/links/00000000-0000-4000-8000-0000000000a3" {
            (
                "200 OK",
                r#"{"player_uuid":"00000000-0000-4000-8000-0000000000a3","player_name":"Alice","did":"did:plc:alice","handle":"alice.test","linked_at":1760000000}"#.to_string(),
            )
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
                "/xrpc/com.atproto.server.getServiceAuth" => {
                    if authorized {
                        (
                            "200 OK",
                            r#"{"token":"stub-service-token"}"#.to_string(),
                        )
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
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
                    // Public repos serve getRecord unauthenticated, which the
                    // M21 interaction reads rely on.
                    let repo = param("repo").unwrap_or_default().replace("%3A", ":");
                    if let Some(record) = records.lock().unwrap().get(&key).cloned() {
                        (
                            "200 OK",
                            format!(
                                r#"{{"uri":"at://{repo}/{key}","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy","value":{record}}}"#
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
                "/xrpc/app.bsky.graph.getFollows" => {
                    // Two pages: cursor "c2" on the first, none on the second.
                    if path.contains("cursor=c2") {
                        (
                            "200 OK",
                            r#"{"follows":[{"did":"did:plc:carol","handle":"carol.test","displayName":"Carol"}]}"#
                                .to_string(),
                        )
                    } else {
                        (
                            "200 OK",
                            r#"{"follows":[{"did":"did:plc:alice","handle":"alice.test","displayName":"Alice"},{"did":"did:plc:bob","handle":"bob.test"}],"cursor":"c2"}"#
                                .to_string(),
                        )
                    }
                }
                "/xrpc/app.bsky.graph.getFollowers" => (
                    "200 OK",
                    r#"{"followers":[{"did":"did:plc:dave","handle":"dave.test"}]}"#
                        .to_string(),
                ),
                "/xrpc/app.bsky.graph.getBlocks" => (
                    "200 OK",
                    r#"{"blocks":[{"did":"did:plc:mallory","handle":"mallory.test"}]}"#
                        .to_string(),
                ),
                "/xrpc/app.bsky.graph.getMutes" => (
                    "200 OK",
                    r#"{"mutes":[]}"#.to_string(),
                ),
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
                "/did:plc:stubuser"
                | "/did:plc:labeler"
                | "/did:plc:community"
                | "/did:plc:remote" => {
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
                // The author of the M21 interaction targets.
                "/did:plc:target" => {
                    let doc = serde_json::json!({
                        "id": "did:plc:target",
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
                    r#"{"did":"did:plc:stubuser","handle":"stub.test","displayName":"AppView Name","description":"AppView bio","pronouns":"she/her","labels":[{"ver":1,"src":"did:plc:labeler","uri":"did:plc:stubuser","val":"player","cts":"2026-11-12T10:00:00.000Z"}]}"#.to_string(),
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
                "/xrpc/com.atproto.repo.deleteRecord" => {
                    if authorized {
                        let body_json: serde_json::Value =
                            serde_json::from_str(&body_string).unwrap_or_default();
                        let collection = body_json["collection"].as_str().unwrap_or("");
                        let rkey = body_json["rkey"].as_str().unwrap_or("");
                        records
                            .lock()
                            .unwrap()
                            .remove(&format!("{collection}/{rkey}"));
                        ("200 OK", r#"{}"#.to_string())
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                "/xrpc/app.bsky.graph.muteActor" | "/xrpc/app.bsky.graph.unmuteActor" => {
                    if authorized {
                        // These procedures have no output schema; the real
                        // service answers with an empty body and no JSON
                        // content type, which the client maps to `()`.
                        ("200 OK", String::new())
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
        // The mute procedures have no output schema: answer with an empty
        // body and no content type so the generated client sees a raw
        // response and maps it to `()`.
        if path.starts_with("/xrpc/app.bsky.graph.muteActor")
            || path.starts_with("/xrpc/app.bsky.graph.unmuteActor")
        {
            let response =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            return;
        }
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
        // Drain the startup events, then shut down.
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedOut))
        });
        let _ = wait_for(&event_rx, |e| matches!(e, Event::JoinGateState { .. }));
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
    fn colibri_requires_approval_reads_the_community_record() {
        let pds = StubPds::start(false);
        pds.seed_record(
            colibri::COMMUNITY_COLLECTION,
            "self",
            serde_json::json!({
                "$type": colibri::COMMUNITY_COLLECTION,
                "requiresApprovalToJoin": true
            }),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(colibri::requires_approval(
            &reqwest::Client::new(),
            &stub_resolver(&pds.url),
            "did:plc:community",
        ));
        assert_eq!(result, Some(true));
        assert!(
            pds.requests()
                .iter()
                .any(|request| request.path.starts_with("/xrpc/com.atproto.repo.getRecord")),
            "the community record is read over getRecord"
        );
    }

    #[test]
    fn colibri_membership_is_created_once() {
        let pds = StubPds::start(false);
        let config = test_config(pds.url.clone());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (agent, _) = runtime
            .block_on(session::login(&config, oauth::new_shared_dpop()))
            .unwrap();
        let channel =
            ChannelRef::parse("at://did:plc:community/social.colibri.channel/3jchannel").unwrap();
        let first = runtime
            .block_on(colibri::ensure_membership(
                &agent,
                "did:plc:stubuser",
                &channel,
                "3jmembership",
            ))
            .unwrap();
        assert!(first, "the first call creates the membership record");
        let second = runtime
            .block_on(colibri::ensure_membership(
                &agent,
                "did:plc:stubuser",
                &channel,
                "3jmembership2",
            ))
            .unwrap();
        assert!(!second, "a second call reuses the existing record");
        let creates: Vec<_> = pds
            .requests()
            .into_iter()
            .filter(|request| {
                request.path == "/xrpc/com.atproto.repo.createRecord"
                    && request.body.contains(colibri::MEMBERSHIP_COLLECTION)
            })
            .collect();
        assert_eq!(creates.len(), 1, "exactly one membership record is written");
        let body: serde_json::Value = serde_json::from_str(&creates[0].body).unwrap();
        assert_eq!(body["record"]["community"], channel.community_uri());
    }

    #[test]
    fn colibri_player_membership_is_created_on_login_and_not_duplicated() {
        let pds = StubPds::start(false);
        pds.seed_record(
            colibri::COMMUNITY_COLLECTION,
            "self",
            serde_json::json!({
                "$type": colibri::COMMUNITY_COLLECTION,
                "requiresApprovalToJoin": true
            }),
        );
        let dir = temp_dir("colibri-player-membership");
        write_config(&dir, &test_config(pds.url.clone()));
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(WorkerOptions::new(dir), events, oauth_tx, colibri_tx);
        worker.resolver = stub_resolver(&pds.url);
        worker.colibri_channel = Some(
            ChannelRef::parse("at://did:plc:community/social.colibri.channel/3jchannel").unwrap(),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(worker.login());

        let mut pending = None;
        while let Ok(event) = event_rx.try_recv() {
            if let Event::ColibriMembershipPending { message } = event {
                pending = Some(message);
            }
        }
        assert_eq!(
            pending.as_deref(),
            Some(colibri::MEMBERSHIP_PENDING_MESSAGE)
        );
        let membership_creates = || {
            pds.requests()
                .into_iter()
                .filter(|request| {
                    request.path == "/xrpc/com.atproto.repo.createRecord"
                        && request.body.contains(colibri::MEMBERSHIP_COLLECTION)
                })
                .count()
        };
        assert_eq!(membership_creates(), 1);

        worker.colibri_player_membership_checked = false;
        runtime.block_on(worker.check_colibri_player_membership());
        assert!(
            event_rx.try_recv().is_err(),
            "an existing player membership emits nothing"
        );
        assert_eq!(membership_creates(), 1, "the membership is not duplicated");
    }

    #[test]
    fn colibri_profile_prompt_is_only_for_a_missing_record() {
        let pds = StubPds::start(false);
        let config = test_config(pds.url.clone());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (agent, _) = runtime
            .block_on(session::login(&config, oauth::new_shared_dpop()))
            .unwrap();
        let dir = temp_dir("colibri-profile");
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(
            WorkerOptions::new(dir.clone()),
            events,
            oauth_tx,
            colibri_tx,
        );
        worker.agent = Some(agent);
        worker.did = Some("did:plc:stubuser".into());
        worker.colibri_channel = Some(
            ChannelRef::parse("at://did:plc:community/social.colibri.channel/3jchannel").unwrap(),
        );

        runtime.block_on(worker.check_colibri_profile());
        match event_rx.try_recv() {
            Ok(Event::ColibriProfileMissing { url }) => assert_eq!(url, colibri::PROFILE_URL),
            other => panic!("expected the Colibri profile prompt, got {other:?}"),
        }
        assert!(
            pds.requests()
                .iter()
                .any(|request| request.path.contains(colibri::ACTOR_PROFILE_COLLECTION)),
            "the profile check reads the actor profile over getRecord"
        );

        pds.seed_record(
            colibri::ACTOR_PROFILE_COLLECTION,
            colibri::SELF_RKEY,
            serde_json::json!({"$type": colibri::ACTOR_PROFILE_COLLECTION}),
        );
        worker.colibri_profile_checked = false;
        runtime.block_on(worker.check_colibri_profile());
        assert!(
            event_rx.try_recv().is_err(),
            "an existing profile emits no prompt"
        );
    }

    #[test]
    fn join_gate_allows_only_a_linked_member_and_fails_closed() {
        let pds = StubPds::start(false);
        let dir = temp_dir("join-gate");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone();
        config.server_api_url = pds.url.clone();
        config.server_link_token = "stub-link-token".into();
        config.join_gate_enabled = true;
        config.join_gate_list = "at://did:plc:owner/app.bsky.graph.list/3abc".into();
        write_config(&dir, &config);
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(
            WorkerOptions::new(dir.clone()),
            events,
            oauth_tx,
            colibri_tx,
        );
        worker.resolver = stub_resolver(&pds.url);
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(worker.check_join_gate(
            1,
            "00000000-0000-4000-8000-0000000000a3",
            "Alice",
        ));
        match event_rx.try_recv() {
            Ok(Event::JoinGateDecision {
                request_id,
                allowed,
                message,
            }) => {
                assert_eq!(request_id, 1);
                assert!(allowed, "alice is linked and on the list");
                assert!(message.is_empty());
            }
            other => panic!("expected an allowed JoinGateDecision, got {other:?}"),
        }

        runtime.block_on(worker.check_join_gate(
            2,
            "00000000-0000-4000-8000-0000000000a2",
            "Unlinked",
        ));
        match event_rx.try_recv() {
            Ok(Event::JoinGateDecision {
                request_id,
                allowed,
                message,
            }) => {
                assert_eq!(request_id, 2);
                assert!(!allowed);
                assert!(message.contains("linked Bluesky account"), "{message}");
                assert!(
                    message.contains("https://links.test/link/browser-token"),
                    "the gate refusal carries the browser link: {message}"
                );
                let requests = pds.requests();
                let browser_request = requests
                    .iter()
                    .find(|request| request.path == "/v1/browser/pairings")
                    .expect("the worker asks the identity service for a browser link");
                let link_body: serde_json::Value =
                    serde_json::from_str(&browser_request.body).unwrap();
                assert_eq!(link_body["ttl_seconds"], 600);
            }
            other => panic!("expected an unlinked refusal, got {other:?}"),
        }

        runtime.block_on(worker.check_join_gate(
            3,
            "00000000-0000-4000-8000-0000000000a1",
            "NotMember",
        ));
        match event_rx.try_recv() {
            Ok(Event::JoinGateDecision {
                request_id,
                allowed,
                message,
            }) => {
                assert_eq!(request_id, 3);
                assert!(!allowed);
                assert!(message.contains("not on this server"), "{message}");
            }
            other => panic!("expected a non-member refusal, got {other:?}"),
        }

        config.join_gate_list.clear();
        write_config(&dir, &config);
        runtime.block_on(worker.check_join_gate(
            4,
            "00000000-0000-4000-8000-0000000000a3",
            "Alice",
        ));
        match event_rx.try_recv() {
            Ok(Event::JoinGateDecision {
                request_id,
                allowed,
                message,
            }) => {
                assert_eq!(request_id, 4);
                assert!(!allowed);
                assert!(message.contains("no whitelist list"), "{message}");
            }
            other => panic!("expected a no-list refusal, got {other:?}"),
        }
    }

    #[test]
    fn join_gate_sweep_kicks_only_linked_non_members() {
        let pds = StubPds::start(false);
        let dir = temp_dir("join-gate-sweep");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone();
        config.server_api_url = pds.url.clone();
        config.join_gate_enabled = true;
        config.join_gate_list = "at://did:plc:owner/app.bsky.graph.list/3abc".into();
        write_config(&dir, &config);
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(
            WorkerOptions::new(dir.clone()),
            events,
            oauth_tx,
            colibri_tx,
        );
        worker.resolver = stub_resolver(&pds.url);
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let alice = SweepPlayer {
            player_uuid: "00000000-0000-4000-8000-0000000000a3".into(),
            player_name: "Alice".into(),
        };
        let remote = SweepPlayer {
            player_uuid: "00000000-0000-4000-8000-0000000000a1".into(),
            player_name: "Remote".into(),
        };
        let unlinked = SweepPlayer {
            player_uuid: "00000000-0000-4000-8000-0000000000a2".into(),
            player_name: "Unlinked".into(),
        };

        // Alice is on the list, Remote is linked but off it, Unlinked has no
        // link. Only Remote is reported for a kick.
        runtime.block_on(worker.join_gate_sweep(vec![
            alice.clone(),
            remote.clone(),
            unlinked.clone(),
        ]));
        match event_rx.try_recv() {
            Ok(Event::JoinGateKicks { players }) => {
                assert_eq!(players, vec![remote.clone()]);
            }
            other => panic!("expected JoinGateKicks, got {other:?}"),
        }

        // A clean sweep (everyone a member or unlinked) is silent.
        runtime.block_on(worker.join_gate_sweep(vec![alice.clone(), unlinked.clone()]));
        assert!(event_rx.try_recv().is_err(), "a clean sweep is silent");

        // An empty player list never even reads the list.
        runtime.block_on(worker.join_gate_sweep(Vec::new()));
        assert!(event_rx.try_recv().is_err(), "an empty sweep is silent");

        // A failed list read kicks nobody: atpcraft's fail-safe.
        config.join_gate_list = "https://example.com/nope".into();
        write_config(&dir, &config);
        runtime.block_on(worker.join_gate_sweep(vec![remote.clone()]));
        assert!(
            event_rx.try_recv().is_err(),
            "a failed list refresh kicks nobody"
        );

        // A disabled gate is a no-op.
        config.join_gate_enabled = false;
        write_config(&dir, &config);
        runtime.block_on(worker.join_gate_sweep(vec![remote.clone()]));
        assert!(event_rx.try_recv().is_err(), "a disabled gate is silent");
    }

    #[test]
    fn join_gate_state_carries_the_floored_sweep_period() {
        let dir = temp_dir("join-gate-state");
        let mut config = test_config("https://pds.example".into());
        config.join_gate_enabled = true;
        config.join_gate_list = "at://did:plc:owner/app.bsky.graph.list/3abc".into();

        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = Worker::new(
            WorkerOptions::new(dir.clone()),
            events,
            oauth_tx,
            colibri_tx,
        );

        // Off stays off.
        config.join_gate_sweep_seconds = 0;
        write_config(&dir, &config);
        worker.emit_join_gate_state();
        match event_rx.try_recv() {
            Ok(Event::JoinGateState { sweep_seconds, .. }) => assert_eq!(sweep_seconds, 0),
            other => panic!("expected JoinGateState, got {other:?}"),
        }

        // A positive value is floored at 60 seconds (atpcraft's floor).
        config.join_gate_sweep_seconds = 5;
        write_config(&dir, &config);
        worker.emit_join_gate_state();
        match event_rx.try_recv() {
            Ok(Event::JoinGateState { sweep_seconds, .. }) => assert_eq!(sweep_seconds, 60),
            other => panic!("expected JoinGateState, got {other:?}"),
        }

        // A larger value passes through.
        config.join_gate_sweep_seconds = 300;
        write_config(&dir, &config);
        worker.emit_join_gate_state();
        match event_rx.try_recv() {
            Ok(Event::JoinGateState { sweep_seconds, .. }) => assert_eq!(sweep_seconds, 300),
            other => panic!("expected JoinGateState, got {other:?}"),
        }
    }

    #[test]
    fn colibri_remote_profile_prompt_targets_the_linked_player() {
        let pds = StubPds::start(false);
        let dir = temp_dir("colibri-remote-profile");
        let mut config = test_config(pds.url.clone());
        config.server_api_url = pds.url.clone();
        write_config(&dir, &config);
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(WorkerOptions::new(dir), events, oauth_tx, colibri_tx);
        worker.resolver = stub_resolver(&pds.url);
        worker.did = Some("did:plc:stubuser".into());
        worker.colibri_channel = Some(
            ChannelRef::parse("at://did:plc:community/social.colibri.channel/3jchannel").unwrap(),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let uuid = "00000000-0000-4000-8000-0000000000a1";

        runtime.block_on(worker.check_remote_colibri_profile(uuid, "Alex"));
        match event_rx.try_recv() {
            Ok(Event::ColibriRemoteProfileMissing {
                player_uuid,
                player_name,
                url,
            }) => {
                assert_eq!(player_uuid, uuid);
                assert_eq!(player_name, "Alex");
                assert_eq!(url, colibri::PROFILE_URL);
            }
            other => panic!("expected the remote profile prompt, got {other:?}"),
        }
        let requests = pds.requests();
        assert!(
            requests
                .iter()
                .any(|request| request.path == format!("/v1/links/{uuid}")),
            "the worker resolves the join through the public link map"
        );
        assert!(
            requests.iter().any(|request| request
                .path
                .starts_with("/xrpc/com.atproto.repo.getRecord")
                && request.path.contains(colibri::ACTOR_PROFILE_COLLECTION)),
            "the linked player's actor profile is read over getRecord"
        );

        runtime.block_on(
            worker.check_remote_colibri_profile("00000000-0000-4000-8000-0000000000a2", "Unlinked"),
        );
        assert!(event_rx.try_recv().is_err(), "an unlinked player is quiet");
    }

    #[test]
    fn check_player_profile_composes_a_linked_players_nickname() {
        let pds = StubPds::start(false);
        let dir = temp_dir("player-profile");
        let mut config = test_config(pds.url.clone());
        config.server_api_url = pds.url.clone();
        config.appview_url = pds.url.clone();
        write_config(&dir, &config);
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(WorkerOptions::new(dir), events, oauth_tx, colibri_tx);
        worker.resolver = stub_resolver(&pds.url);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let uuid = "00000000-0000-4000-8000-0000000000a1";

        runtime.block_on(worker.check_player_profile(uuid, "Alex"));
        match event_rx.try_recv() {
            Ok(Event::PlayerProfile {
                player_uuid,
                player_name,
                nickname,
            }) => {
                assert_eq!(player_uuid, uuid);
                assert_eq!(player_name, "Alex");
                assert_eq!(nickname, "AppView_Name (she/her)");
            }
            other => panic!("expected PlayerProfile, got {other:?}"),
        }
        let requests = pds.requests();
        assert!(
            requests
                .iter()
                .any(|request| request.path == format!("/v1/links/{uuid}")),
            "the worker resolves the join through the public link map"
        );
        assert!(
            requests
                .iter()
                .any(|request| request.path.starts_with("/xrpc/app.bsky.actor.getProfile")),
            "the linked player's profile is read from the AppView"
        );

        runtime.block_on(
            worker.check_player_profile("00000000-0000-4000-8000-0000000000a2", "Unlinked"),
        );
        assert!(event_rx.try_recv().is_err(), "an unlinked player is quiet");
    }

    #[test]
    fn colibri_send_limit_allows_the_sixty_second_boundary() {
        let start = Instant::now();
        let mut hits = VecDeque::new();
        for offset in 0..COLIBRI_SEND_LIMIT {
            assert!(colibri_send_allowed(
                &mut hits,
                start + Duration::from_millis(offset as u64)
            ));
        }
        assert!(!colibri_send_allowed(
            &mut hits,
            start + Duration::from_secs(59)
        ));
        assert!(
            colibri_send_allowed(&mut hits, start + Duration::from_secs(60)),
            "the oldest hit expires at the 60-second boundary"
        );
        assert!(!colibri_send_allowed(
            &mut hits,
            start + Duration::from_secs(60)
        ));
    }

    #[test]
    fn colibri_explicit_send_reports_the_rate_limit() {
        let dir = temp_dir("colibri-rate-limit");
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, _colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(WorkerOptions::new(dir), events, oauth_tx, colibri_tx);
        worker.did = Some("did:plc:stubuser".into());
        worker.colibri_rate_did = worker.did.clone();
        worker.colibri_channel = Some(
            ChannelRef::parse("at://did:plc:community/social.colibri.channel/3jchannel").unwrap(),
        );
        for _ in 0..COLIBRI_SEND_LIMIT {
            worker.colibri_send_times.push_back(Instant::now());
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(worker.send_colibri("blocked", false));
        match event_rx.try_recv() {
            Ok(Event::Failed { message }) => assert!(message.contains("rate limit"), "{message}"),
            other => panic!("expected the rate-limit failure, got {other:?}"),
        }
    }

    #[test]
    fn colibri_reconnect_without_a_bridge_fails_cleanly() {
        let dir = temp_dir("colibri-reconnect-off");
        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::ColibriReconnect).unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("not running")),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    // The tungstenite handshake callback owns its fixed Response error type;
    // boxing it is not possible without changing the server API.
    #[allow(clippy::result_large_err)]
    #[test]
    fn colibri_reconnect_remints_without_repairing_membership() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::http::{HeaderValue, header::SEC_WEBSOCKET_PROTOCOL};

        let pds = StubPds::start(false);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let appview = runtime.spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                socket,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(
                        request
                            .headers()
                            .get(SEC_WEBSOCKET_PROTOCOL)
                            .and_then(|value| value.to_str().ok()),
                        Some("colibri.auth.bearer, stub-service-token")
                    );
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        HeaderValue::from_static("colibri.auth.bearer"),
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let _ = socket.next().await;
        });

        let dir = temp_dir("colibri-reconnect");
        let mut config = test_config(pds.url.clone());
        config.colibri_appview_url = format!("http://{addr}");
        write_config(&dir, &config);
        let (agent, _) = runtime
            .block_on(session::login(&config, oauth::new_shared_dpop()))
            .unwrap();
        let (events, event_rx) = std::sync::mpsc::channel();
        let (oauth_tx, _oauth_rx) = tokio::sync::mpsc::unbounded_channel();
        let (colibri_tx, mut colibri_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = Worker::new(WorkerOptions::new(dir), events, oauth_tx, colibri_tx);
        worker.server_agent = Some(agent);
        worker.server_did = Some("did:plc:stubuser".into());
        worker.colibri_channel = Some(
            ChannelRef::parse("at://did:plc:community/social.colibri.channel/3jchannel").unwrap(),
        );
        worker.colibri_profile_checked = true;

        runtime.block_on(async {
            worker.reconnect_colibri().await;
            let connected = tokio::time::timeout(Duration::from_secs(5), colibri_rx.recv())
                .await
                .expect("the reconnected stream connects")
                .expect("a connection event");
            assert_eq!(connected, StreamEvent::Connected);
        });
        match event_rx.try_recv() {
            Ok(Event::Colibri {
                active: true,
                connected: false,
                channel,
                community,
                error: None,
            }) => {
                assert_eq!(
                    channel,
                    "at://did:plc:community/social.colibri.channel/3jchannel"
                );
                assert_eq!(
                    community,
                    "at://did:plc:community/social.colibri.community/self"
                );
            }
            other => panic!("expected the reconnected bridge status, got {other:?}"),
        }
        let requests = pds.requests();
        assert!(
            requests.iter().any(|request| request
                .path
                .starts_with("/xrpc/com.atproto.server.getServiceAuth")),
            "reconnect mints a fresh service-auth token"
        );
        assert!(
            !requests.iter().any(|request| {
                request.body.contains(colibri::MEMBERSHIP_COLLECTION)
                    || request.path.contains(colibri::MEMBERSHIP_COLLECTION)
            }),
            "reconnect does not repair membership"
        );
        worker.colibri_task.take().unwrap().abort();
        appview.abort();
    }

    #[test]
    fn colibri_start_requires_the_server_account() {
        let dir = temp_dir("colibri-no-server");
        let mut config = test_config("https://pds.example".into());
        config.colibri_channel_uri = "at://did:plc:abc/social.colibri.channel/3jtest".into();
        write_config(&dir, &config);
        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx.send(Command::ColibriStart).unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("server account")),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn colibri_explicit_send_without_a_bridge_fails_cleanly() {
        let dir = temp_dir("colibri-not-running");
        let (command_tx, event_rx, handle) = spawn_worker(dir);
        command_tx
            .send(Command::ColibriSend {
                text: "hello".into(),
                automatic: false,
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("not running")),
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
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
            list_cache_seconds: 300,
            server_account_token_refresh_seconds: 30,
            post_death_messages: true,
            post_chat_messages: true,
            chat_server_id: "my-server".into(),
            persist_session: true,
            oauth_client_id: String::new(),
            oauth_scope: String::new(),
            skin_path: String::new(),
            cape_path: String::new(),
            skin_model: "default".into(),
            client_name: "atmospheric-horizon".into(),
            server_name: String::new(),
            server_description: String::new(),
            server_address: String::new(),
            server_website: String::new(),
            server_contact: String::new(),
            server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
            game_version: "26.3".into(),
            appview_url: "https://public.api.bsky.app".into(),
            server_api_url: String::new(),
            server_link_token: String::new(),
            browser_link_ttl_seconds: 900,
            join_gate_enabled: false,
            join_gate_list: String::new(),
            join_gate_sweep_seconds: 90,
            trusted_server_keys: Vec::new(),
            colibri_bridge_enabled: false,
            colibri_channel_uri: String::new(),
            colibri_appview_url: "https://api.colibri.social".into(),
            colibri_appview_did: "did:web:api.colibri.social".into(),
            colibri_server_events_enabled: true,
            colibri_server_events_fallback_enabled: true,
        };
        command_tx
            .send(Command::SaveConfig {
                config: Box::new(config.clone()),
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
        assert_eq!(back.join_gate_sweep_seconds, 90);
        assert_eq!(back.browser_link_ttl_seconds, 900);
    }

    #[test]
    fn server_profile_name_falls_back_to_the_client_name() {
        let mut config = Config::default();
        assert_eq!(server_profile_name(&config), "atmospheric-horizon");
        config.client_name = "Custom Client".into();
        assert_eq!(server_profile_name(&config), "Custom Client");
        config.server_name = "My Server".into();
        assert_eq!(server_profile_name(&config), "My Server");
    }

    #[test]
    fn server_profile_capabilities_follow_atpcraft_feature_rules() {
        let mut config = Config::default();
        assert_eq!(server_profile_capabilities(&config), vec!["skin-sync"]);
        config.join_gate_enabled = true;
        assert_eq!(
            server_profile_capabilities(&config),
            vec!["join-gate", "skin-sync"]
        );
        config.server_capabilities = vec!["labeler".into(), "skin-sync".into()];
        assert_eq!(
            server_profile_capabilities(&config),
            vec!["labeler", "skin-sync"]
        );
    }

    #[test]
    fn list_cache_freshness_honors_ttl_and_key() {
        let now = Instant::now();
        let cache = ListCache {
            appview: "https://appview.example".into(),
            list: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
            members: vec!["did:plc:alice".into()],
            fetched_at: now,
        };
        assert!(cache.is_fresh(
            "https://appview.example",
            "at://did:plc:owner/app.bsky.graph.list/3abc",
            300,
            now + Duration::from_secs(299)
        ));
        assert!(!cache.is_fresh(
            "https://appview.example",
            "at://did:plc:owner/app.bsky.graph.list/3abc",
            300,
            now + Duration::from_secs(300)
        ));
        assert!(!cache.is_fresh(
            "https://appview.example",
            "at://did:plc:owner/app.bsky.graph.list/3abc",
            0,
            now
        ));
        assert!(!cache.is_fresh(
            "https://other.example",
            "at://did:plc:owner/app.bsky.graph.list/3abc",
            300,
            now
        ));
        assert!(!cache.is_fresh(
            "https://appview.example",
            "at://did:plc:owner/app.bsky.graph.list/other",
            300,
            now
        ));
    }

    #[test]
    fn server_token_refresh_period_is_floored_at_one_second() {
        assert_eq!(server_token_refresh_period(0), Duration::from_secs(1));
        assert_eq!(server_token_refresh_period(1), Duration::from_secs(1));
        assert_eq!(server_token_refresh_period(60), Duration::from_secs(60));
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
    fn post_chat_uses_the_signing_key_id_when_no_override_is_configured() {
        let pds = StubPds::start(false);
        let dir = temp_dir("post-chat-key-id");
        let mut config = test_config(pds.url.clone());
        config.post_chat_messages = true;
        config.chat_server_id = String::new();
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        command_tx
            .send(Command::PostChat {
                message: "chat with the server key".into(),
                automatic: false,
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));
        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();

        let key_file: signing::SigningKeyFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SIGNING_KEY_FILE)).unwrap())
                .unwrap();
        let requests = pds.requests();
        let create_record = requests
            .iter()
            .find(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .expect("createRecord was called");
        let body: serde_json::Value = serde_json::from_str(&create_record.body).unwrap();
        assert_eq!(body["record"]["server"], key_file.key_id);
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
        // A fresh cache hit reuses the members without another request.
        command_tx
            .send(Command::FetchList {
                reference: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ListMembers { .. })) {
            Event::ListMembers { members, .. } => assert_eq!(members.len(), 3),
            other => panic!("expected the cached ListMembers, got {other:?}"),
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
    fn check_gate_decides_membership_and_fails_closed() {
        let pds = StubPds::start(false);
        let dir = temp_dir("check-gate");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone(); // the stub doubles as the AppView
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));
        // did:plc:alice is on the stub list (two pages: alice, bob, carol).
        command_tx
            .send(Command::CheckGate {
                list: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
                subject: "did:plc:alice".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::GateDecision { .. })) {
            Event::GateDecision {
                list,
                subject,
                allowed,
            } => {
                assert_eq!(list, "at://did:plc:owner/app.bsky.graph.list/3abc");
                assert_eq!(subject, "did:plc:alice");
                assert!(allowed);
            }
            other => panic!("expected GateDecision, got {other:?}"),
        }
        let get_list_hits = |pds: &StubPds| {
            pds.requests()
                .iter()
                .filter(|r| r.path.starts_with("/xrpc/app.bsky.graph.getList"))
                .count()
        };
        assert_eq!(get_list_hits(&pds), 2, "the first read fetched two pages");

        // A fresh cache miss refreshes once before the final reject, matching
        // atpcraft's `isMember`.
        command_tx
            .send(Command::CheckGate {
                list: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
                subject: "did:plc:mallory".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::GateDecision { .. })) {
            Event::GateDecision { allowed, .. } => assert!(!allowed),
            other => panic!("expected GateDecision, got {other:?}"),
        }
        assert_eq!(get_list_hits(&pds), 4, "the fresh miss refreshed two pages");

        // A bsky.app URL list (handle owner resolves via the stub to
        // did:plc:stubuser) and a handle subject (also did:plc:stubuser,
        // which is not on the list) → denied.
        command_tx
            .send(Command::CheckGate {
                list: "https://bsky.app/profile/owner.test/lists/3abc".into(),
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::GateDecision { .. })) {
            Event::GateDecision {
                list,
                subject,
                allowed,
            } => {
                assert_eq!(
                    list, "at://did:plc:stubuser/app.bsky.graph.list/3abc",
                    "the URL's handle owner was resolved"
                );
                assert_eq!(subject, "did:plc:stubuser");
                assert!(!allowed);
            }
            other => panic!("expected GateDecision, got {other:?}"),
        }
        assert_eq!(get_list_hits(&pds), 6, "the second list was fetched fresh");
        // The getList reads went out unauthenticated.
        assert!(
            pds.requests()
                .iter()
                .filter(|r| r.path.starts_with("/xrpc/app.bsky.graph.getList"))
                .all(|r| !r.authorized),
            "the gate read is unauthenticated"
        );

        // Fail closed: a record-URI subject or a bad list reference is an
        // error (the deny-with-reason), never a silent allow.
        command_tx
            .send(Command::CheckGate {
                list: "at://did:plc:owner/app.bsky.graph.list/3abc".into(),
                subject: "at://did:plc:alice/app.bsky.feed.post/1".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("want an account"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        command_tx
            .send(Command::CheckGate {
                list: "https://example.com/nope".into(),
                subject: "did:plc:alice".into(),
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
    }

    #[test]
    fn fetch_chat_reads_the_chat_history_unauthenticated() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-chat");
        write_config(&dir, &test_config(pds.url.clone()));
        // A chat record seeded directly, as if a previous session posted it.
        pds.seed_record(
            records::CHAT_COLLECTION,
            "3jub4stub0001",
            serde_json::json!({
                "$type": records::CHAT_COLLECTION,
                "sentAt": "2026-11-12T10:00:00.000Z",
                "message": "seeded earlier",
                "server": "test-server",
            }),
        );
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        // Logged out: the read is unauthenticated and finds the seeded record.
        command_tx
            .send(Command::FetchChat {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ChatHistory { .. })) {
            Event::ChatHistory {
                subject,
                via,
                total,
                items,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                assert_eq!(total, 1);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].message, "seeded earlier");
                assert_eq!(items[0].server, "test-server");
                assert_eq!(
                    items[0].at_uri,
                    "at://did:plc:stubuser/at.atpcraft.player.chat/3jub4stub0001"
                );
            }
            other => panic!("expected ChatHistory, got {other:?}"),
        }
        assert!(
            pds.requests()
                .iter()
                .filter(|r| r.path.starts_with("/xrpc/com.atproto.repo.listRecords"))
                .all(|r| !r.authorized),
            "the chat read is unauthenticated"
        );

        // Log in and post two chat records, then read the history back.
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        for message in ["hello", "world"] {
            command_tx
                .send(Command::PostChat {
                    message: message.into(),
                    automatic: false,
                })
                .unwrap();
            let _ = wait_for(&event_rx, |e| matches!(e, Event::Posted { .. }));
        }
        command_tx
            .send(Command::FetchChat {
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ChatHistory { .. })) {
            Event::ChatHistory { total, items, .. } => {
                assert_eq!(total, 3);
                assert_eq!(items.len(), 3, "under the HUD limit all are shown");
                assert_eq!(items[0].message, "seeded earlier");
                assert_eq!(items[1].message, "hello");
                assert_eq!(items[2].message, "world", "ordered by sentAt");
                // TID rkeys order the same way as sentAt.
                assert!(items[0].rkey < items[1].rkey && items[1].rkey < items[2].rkey);
            }
            other => panic!("expected ChatHistory, got {other:?}"),
        }

        // A repo with no chat records reports zero; a record URI fails before
        // any network call.
        command_tx
            .send(Command::FetchChat {
                subject: "did:plc:empty".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ChatHistory { .. })) {
            Event::ChatHistory { total, items, .. } => {
                assert_eq!(total, 0);
                assert!(items.is_empty());
            }
            other => panic!("expected ChatHistory, got {other:?}"),
        }
        let before = pds.non_poll_requests().len();
        command_tx
            .send(Command::FetchChat {
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
            pds.non_poll_requests().len(),
            before,
            "no HTTP call for a record URI"
        );

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fetch_records_reads_any_collection_unauthenticated() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-records");
        write_config(&dir, &test_config(pds.url.clone()));
        // A record in a collection the mod does not otherwise know about.
        pds.seed_record(
            "app.bsky.feed.post",
            "3jub4stub0007",
            serde_json::json!({
                "$type": "app.bsky.feed.post",
                "text": "a generic record",
                "createdAt": "2026-11-12T10:00:00.000Z",
            }),
        );
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        // Logged out: the read is unauthenticated and finds the seeded record.
        command_tx
            .send(Command::FetchRecords {
                subject: "did:plc:stubuser".into(),
                collection: "app.bsky.feed.post".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::RecordsHistory { .. })) {
            Event::RecordsHistory {
                subject,
                via,
                collection,
                total,
                items,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                assert_eq!(collection, "app.bsky.feed.post");
                assert_eq!(total, 1);
                assert_eq!(items.len(), 1);
                assert!(items[0].contains("a generic record"), "{}", items[0]);
            }
            other => panic!("expected RecordsHistory, got {other:?}"),
        }
        assert!(
            pds.requests()
                .iter()
                .filter(|r| r.path.starts_with("/xrpc/com.atproto.repo.listRecords"))
                .all(|r| !r.authorized),
            "the generic record read is unauthenticated"
        );

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
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
        let before = pds.non_poll_requests().len();
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
            pds.non_poll_requests().len(),
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
        let before = pds.non_poll_requests().len();
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
        assert_eq!(
            pds.non_poll_requests().len(),
            before,
            "no HTTP call when logged out"
        );

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

    #[test]
    fn presence_fallback_uses_the_five_second_window() {
        let at = Instant::now();
        assert!(presence_notice_needed(None, None, None, "join", at));
        assert!(!presence_notice_needed(
            Some(at + COLIBRI_PRESENCE_WINDOW),
            None,
            None,
            "join",
            at
        ));
        assert!(presence_notice_needed(
            Some(at + COLIBRI_PRESENCE_WINDOW + Duration::from_nanos(1)),
            None,
            None,
            "join",
            at
        ));
        assert!(!presence_notice_needed(
            None,
            None,
            Some(at + Duration::from_secs(4)),
            "leave",
            at
        ));
        assert!(presence_notice_needed(None, Some(at), None, "join", at));
        assert!(!presence_notice_needed(None, None, None, "death", at));
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
        let before = pds.non_poll_requests().len();
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
            pds.non_poll_requests().len(),
            before,
            "no HTTP call for a bad subject"
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
    fn fetch_profile_applies_atpcraft_pronoun_precedence() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-profile");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone();
        write_config(&dir, &config);
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        // With no direct record, the AppView profile is the fallback.
        command_tx
            .send(Command::FetchProfile {
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Profile { .. })) {
            Event::Profile {
                subject,
                via,
                profile,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                assert_eq!(profile.handle, "stub.test");
                assert_eq!(profile.display_name, "AppView Name");
                assert_eq!(profile.description, "AppView bio");
                assert_eq!(profile.pronouns.as_deref(), Some("she/her"));
            }
            other => panic!("expected Profile, got {other:?}"),
        }

        // A successful direct record wins for pronouns, even when it is not
        // the AppView value; the AppView display name still wins.
        pds.seed_record(
            profiles::PROFILE_COLLECTION,
            profiles::PROFILE_RKEY,
            serde_json::json!({
                "displayName": "Direct Name",
                "description": "Direct bio",
                "pronouns": "  they / them  ",
            }),
        );
        command_tx
            .send(Command::FetchProfile {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Profile { .. })) {
            Event::Profile { profile, .. } => {
                assert_eq!(profile.display_name, "AppView Name");
                assert_eq!(profile.pronouns.as_deref(), Some("they / them"));
            }
            other => panic!("expected Profile, got {other:?}"),
        }

        // A successful direct read without pronouns clears the AppView value.
        pds.seed_record(
            profiles::PROFILE_COLLECTION,
            profiles::PROFILE_RKEY,
            serde_json::json!({ "displayName": "Direct Name" }),
        );
        command_tx
            .send(Command::FetchProfile {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Profile { .. })) {
            Event::Profile { profile, .. } => {
                assert_eq!(profile.pronouns, None);
                assert_eq!(profile.display_name, "AppView Name");
            }
            other => panic!("expected Profile, got {other:?}"),
        }

        // Both reads went out unauthenticated.
        let hits: Vec<_> = pds
            .requests()
            .into_iter()
            .filter(|r| {
                r.path.starts_with("/xrpc/app.bsky.actor.getProfile")
                    || r.path
                        .starts_with("/xrpc/com.atproto.repo.getRecord?repo=did%3Aplc%3Astubuser")
            })
            .collect();
        assert!(
            hits.iter()
                .any(|r| { r.path.starts_with("/xrpc/app.bsky.actor.getProfile") })
        );
        assert!(
            hits.iter()
                .any(|r| { r.path.contains("collection=app.bsky.actor.profile") })
        );
        assert!(
            hits.iter().all(|r| !r.authorized),
            "profile reads are unauthenticated"
        );

        // A record URI is not an account: fails before any network call.
        let before = pds.non_poll_requests().len();
        command_tx
            .send(Command::FetchProfile {
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
            pds.non_poll_requests().len(),
            before,
            "no HTTP call for a bad subject"
        );

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
        let before = pds.non_poll_requests().len();
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
            pds.non_poll_requests().len(),
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
    fn fetch_server_profile_reads_and_verifies_descriptor() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-server-profile");
        let foreign = signing::generate();
        let foreign_spki = signing::public_key_spki_b64(&foreign.verifying_key());
        let mut config = test_config(pds.url.clone());
        config.trusted_server_keys = vec![crate::config::TrustedServerKey {
            key_id: "trusted-srv".into(),
            public_key: foreign_spki.clone(),
        }];
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        // Empty repo reports none.
        command_tx
            .send(Command::FetchServerProfile {
                subject: "did:plc:empty".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ServerProfile { .. })) {
            Event::ServerProfile {
                subject, profile, ..
            } => {
                assert_eq!(subject, "did:plc:empty");
                assert!(profile.is_none());
            }
            other => panic!("expected ServerProfile, got {other:?}"),
        }

        // Seed a valid server profile record at rkey "self".
        let record = serde_json::json!({
            "$type": records::SERVER_PROFILE_COLLECTION,
            "name": "Stub Server",
            "description": "A test server",
            "address": "stub.example:25565",
            "minecraftVersion": "26.3",
            "atpcraftVersion": "0.1.0",
            "signingKey": {
                "keyId": "trusted-srv",
                "publicKey": foreign_spki,
                "alg": "ed25519",
            },
            "capabilities": ["player-data-sync"],
            "operator": {
                "handle": "operator.test",
            },
            "updatedAt": "2026-11-13T00:00:00.000Z",
        });
        pds.seed_record(records::SERVER_PROFILE_COLLECTION, "self", record.clone());

        // Read it back by handle (works logged out).
        command_tx
            .send(Command::FetchServerProfile {
                subject: "stub.test".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ServerProfile { .. })) {
            Event::ServerProfile {
                subject,
                via,
                profile,
            } => {
                assert_eq!(subject, "did:plc:stubuser");
                assert_eq!(via, pds.url);
                let d = profile.expect("a server profile exists");
                assert_eq!(d.name, "Stub Server");
                assert_eq!(d.address, "stub.example:25565");
                assert_eq!(d.minecraft_version, "26.3");
                assert_eq!(d.atpcraft_version, "0.1.0");
                assert_eq!(d.capabilities, vec!["player-data-sync"]);
                assert_eq!(d.operator_handle, "operator.test");
                assert_eq!(
                    d.verdict,
                    servers::ServerVerdict::Trusted {
                        key_id: "trusted-srv".into()
                    }
                );
            }
            other => panic!("expected ServerProfile, got {other:?}"),
        }

        // The listRecords call went out unauthenticated against the subject's PDS.
        let list_hit = pds
            .requests()
            .into_iter()
            .find(|r| {
                r.path.starts_with("/xrpc/com.atproto.repo.listRecords")
                    && r.path.contains("collection=at.atpcraft.server.profile")
            })
            .expect("listRecords was called for server profile");
        assert!(!list_hit.authorized, "the read is unauthenticated");

        // An unknown key reports Unknown verdict.
        let mut unknown_rec = record.clone();
        unknown_rec["signingKey"]["keyId"] = serde_json::json!("unknown-key");
        pds.seed_record(records::SERVER_PROFILE_COLLECTION, "self", unknown_rec);
        command_tx
            .send(Command::FetchServerProfile {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ServerProfile { .. })) {
            Event::ServerProfile { profile, .. } => {
                let d = profile.expect("a server profile exists");
                assert_eq!(d.verdict, servers::ServerVerdict::Unknown);
            }
            other => panic!("expected ServerProfile, got {other:?}"),
        }

        // A mismatched public key reports KeyMismatch verdict.
        let other_key = signing::generate();
        let other_spki = signing::public_key_spki_b64(&other_key.verifying_key());
        let mut mismatch_rec = record.clone();
        mismatch_rec["signingKey"]["publicKey"] = serde_json::json!(other_spki);
        pds.seed_record(records::SERVER_PROFILE_COLLECTION, "self", mismatch_rec);
        command_tx
            .send(Command::FetchServerProfile {
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::ServerProfile { .. })) {
            Event::ServerProfile { profile, .. } => {
                let d = profile.expect("a server profile exists");
                assert_eq!(
                    d.verdict,
                    servers::ServerVerdict::KeyMismatch {
                        key_id: "trusted-srv".into()
                    }
                );
            }
            other => panic!("expected ServerProfile, got {other:?}"),
        }

        // A record URI is not a server account reference: fails before any network call.
        let before = pds.non_poll_requests().len();
        command_tx
            .send(Command::FetchServerProfile {
                subject: "at://did:plc:stubuser/app.bsky.feed.post/1".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("want a server account"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(pds.non_poll_requests().len(), before);

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn claim_server_link_posts_the_logged_in_identity() {
        let pds = StubPds::start(false);
        let dir = temp_dir("claim-link");
        let mut config = test_config(pds.url.clone());
        config.server_api_url = pds.url.clone();
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) = spawn_worker(dir.clone());
        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });
        command_tx
            .send(Command::ClaimServerLink {
                code: "ABCD-efgh".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Status(_))) {
            Event::Status(message) => {
                assert!(message.contains("linked Alex"), "{message}");
                assert!(
                    message.contains("00000000-0000-4000-8000-000000000001"),
                    "{message}"
                );
            }
            other => panic!("expected Status, got {other:?}"),
        }

        let requests = pds.requests();
        let claim = requests
            .iter()
            .find(|request| request.path == "/v1/pairings/claim")
            .expect("the claim reached the configured identity service");
        let body: serde_json::Value = serde_json::from_str(&claim.body).unwrap();
        assert_eq!(body["code"], "ABCD-efgh");
        assert_eq!(body["did"], "did:plc:stubuser");
        assert_eq!(body["handle"], "stub.test");

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
    fn server_account_oauth_is_separate_and_logs_out() {
        let stub = StubAuthServer::start(false);
        let dir = temp_dir("server-account");
        let (command_tx, event_rx, handle) = spawn_worker_at(dir.clone(), &stub);

        command_tx
            .send(Command::LoginServerOAuth {
                handle: "stub.test".into(),
            })
            .unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(
                e,
                Event::ServerAccount {
                    state: SessionState::LoggingIn,
                    ..
                }
            )
        });
        let authorize_url = simulate_browser(&event_rx);
        assert!(authorize_url.starts_with(&format!("{}/authorize?", stub.url)));
        match wait_for(&event_rx, |e| {
            matches!(
                e,
                Event::ServerAccount {
                    state: SessionState::LoggedIn { .. },
                    ..
                }
            )
        }) {
            Event::ServerAccount {
                state: SessionState::LoggedIn { did },
                handle,
                ..
            } => {
                assert_eq!(did, "did:plc:stubuser");
                assert_eq!(handle.as_deref(), Some("stub.test"));
            }
            other => panic!("expected ServerAccount(LoggedIn), got {other:?}"),
        }

        // The server session is persisted on its own; the player session is
        // untouched and no app password is written.
        assert!(dir.join(SERVER_SESSION_FILE).is_file());
        assert!(!dir.join(SESSION_FILE).exists());
        let raw = std::fs::read_to_string(dir.join(SERVER_SESSION_FILE)).unwrap();
        assert!(!raw.contains("password"));
        let stored: session::StoredSession = serde_json::from_str(&raw).unwrap();
        assert_eq!(stored.refresh_jwt, "stub-oauth-refresh-1");
        assert!(stored.oauth.is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(SERVER_SESSION_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "server-session.json must be 0600");
        }

        command_tx.send(Command::ServerAccountStatus).unwrap();
        assert!(matches!(
            wait_for(&event_rx, |e| matches!(e, Event::ServerAccount { .. })),
            Event::ServerAccount {
                state: SessionState::LoggedIn { .. },
                ..
            }
        ));

        // Login already auto-published the descriptor (M51); the manual
        // command refreshes it again through the shared writer.
        command_tx.send(Command::PostServerProfileAsServer).unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => {
                assert_eq!(uri, "at://did:plc:stubuser/at.atpcraft.server.profile/self")
            }
            other => panic!("expected Posted, got {other:?}"),
        }
        let puts: Vec<_> = stub
            .requests()
            .into_iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.putRecord")
            .collect();
        assert_eq!(
            puts.len(),
            2,
            "automatic and explicit server-profile upserts"
        );
        assert_eq!(
            puts[0].authorization.as_deref(),
            Some("DPoP stub-oauth-access-1")
        );
        let put: serde_json::Value = serde_json::from_str(&puts[0].body).unwrap();
        assert_eq!(put["collection"], "at.atpcraft.server.profile");
        assert_eq!(put["rkey"], "self");
        assert_eq!(put["record"]["$type"], "at.atpcraft.server.profile");

        command_tx.send(Command::LogoutServerAccount).unwrap();
        match wait_for(&event_rx, |e| {
            matches!(
                e,
                Event::ServerAccount {
                    state: SessionState::LoggedOut,
                    ..
                }
            )
        }) {
            Event::ServerAccount { did, handle, .. } => {
                assert!(did.is_none());
                assert!(handle.is_none());
            }
            other => panic!("expected ServerAccount(LoggedOut), got {other:?}"),
        }
        assert!(!dir.join(SERVER_SESSION_FILE).exists());

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn startup_resumes_server_account_and_publishes_profile() {
        let stub = StubAuthServer::start(false);
        let dir = temp_dir("server-profile-resume");
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
            dir.join(SERVER_SESSION_FILE),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();

        let (command_tx, event_rx, handle) = spawn_worker_at(dir, &stub);
        match wait_for(&event_rx, |e| {
            matches!(
                e,
                Event::ServerAccount {
                    state: SessionState::LoggedIn { .. },
                    ..
                }
            )
        }) {
            Event::ServerAccount {
                state: SessionState::LoggedIn { did },
                ..
            } => assert_eq!(did, "did:plc:stubuser"),
            other => panic!("expected ServerAccount(LoggedIn), got {other:?}"),
        }

        // The automatic publish has no success event; wait for the PDS write.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && !stub
                .requests()
                .iter()
                .any(|r| r.path == "/xrpc/com.atproto.repo.putRecord")
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let puts: Vec<_> = stub
            .requests()
            .into_iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.putRecord")
            .collect();
        assert_eq!(puts.len(), 1, "startup auto-published once");
        assert_eq!(
            puts[0].authorization.as_deref(),
            Some("DPoP stub-oauth-access-2")
        );
        let put: serde_json::Value = serde_json::from_str(&puts[0].body).unwrap();
        assert_eq!(put["collection"], "at.atpcraft.server.profile");
        assert_eq!(put["rkey"], "self");
        assert_eq!(put["record"]["name"], "atmospheric-horizon");
        assert_eq!(
            put["record"]["capabilities"],
            serde_json::json!(["skin-sync"])
        );

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
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

    #[test]
    fn fetch_graph_reads_each_relation() {
        let pds = StubPds::start(false);
        let dir = temp_dir("fetch-graph");
        let mut config = test_config(pds.url.clone());
        config.appview_url = pds.url.clone(); // the stub doubles as the AppView
        write_config(&dir, &config);

        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        for (kind, expected) in [
            ("follows", 3usize),
            ("followers", 1),
            ("blocks", 1),
            ("mutes", 0),
        ] {
            command_tx
                .send(Command::FetchGraph {
                    kind: kind.into(),
                    subject: "stub.test".into(),
                })
                .unwrap();
            match wait_for(&event_rx, |e| matches!(e, Event::SocialGraph { .. })) {
                Event::SocialGraph {
                    kind: got,
                    subject,
                    via,
                    members,
                } => {
                    assert_eq!(got, kind);
                    assert_eq!(subject, "did:plc:stubuser");
                    assert_eq!(via, pds.url);
                    assert_eq!(members.len(), expected, "{kind}: {members:?}");
                    if kind == "follows" {
                        assert_eq!(members[0].handle, "alice.test");
                        assert_eq!(members[0].display_name, "Alice");
                        assert_eq!(members[2].did, "did:plc:carol");
                    }
                }
                other => panic!("expected SocialGraph, got {other:?}"),
            }
        }

        // Every read went out unauthenticated (follows paged twice).
        let graph_hits: Vec<_> = pds
            .requests()
            .into_iter()
            .filter(|r| r.path.starts_with("/xrpc/app.bsky.graph.get"))
            .collect();
        assert_eq!(graph_hits.len(), 5, "follows paged twice, the rest once");
        assert!(graph_hits.iter().all(|r| !r.authorized));

        // An unknown kind fails before any HTTP call.
        let before = pds.requests().len();
        command_tx
            .send(Command::FetchGraph {
                kind: "likes".into(),
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("unknown graph kind"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            pds.requests().len(),
            before,
            "no HTTP call for an unknown kind"
        );

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn graph_writes_create_delete_and_mute_relationships() {
        let pds = StubPds::start(false);
        let dir = temp_dir("graph-writes");
        write_config(&dir, &test_config(pds.url.clone()));
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        // Seed an existing follow and block, so unfollow/unblock have a
        // matching record to delete in addition to the one each create makes.
        let target = "did:plc:target";
        pds.seed_record(
            graph::FOLLOW_COLLECTION,
            "oldfollow",
            serde_json::json!({"$type": graph::FOLLOW_COLLECTION, "subject": target}),
        );
        pds.seed_record(
            graph::BLOCK_COLLECTION,
            "oldblock",
            serde_json::json!({"$type": graph::BLOCK_COLLECTION, "subject": target}),
        );

        for action in ["follow", "unfollow", "block", "unblock", "mute", "unmute"] {
            command_tx
                .send(Command::GraphWrite {
                    action: action.into(),
                    subject: target.into(),
                })
                .unwrap();
            match wait_for(&event_rx, |e| {
                matches!(e, Event::GraphChanged { .. } | Event::Failed { .. })
            }) {
                Event::GraphChanged {
                    action: got,
                    subject,
                } => {
                    assert_eq!(got, action);
                    assert_eq!(subject, target);
                }
                other => panic!("expected GraphChanged for {action}, got {other:?}"),
            }
        }

        let requests = pds.non_poll_requests();
        let creates: Vec<serde_json::Value> = requests
            .iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect();
        let follow = creates
            .iter()
            .find(|body| body["collection"] == graph::FOLLOW_COLLECTION)
            .expect("a follow record was created");
        assert_eq!(follow["record"]["$type"], graph::FOLLOW_COLLECTION);
        assert_eq!(follow["record"]["subject"], target);
        assert!(follow["record"]["createdAt"].is_string());
        let block = creates
            .iter()
            .find(|body| body["collection"] == graph::BLOCK_COLLECTION)
            .expect("a block record was created");
        assert_eq!(block["record"]["$type"], graph::BLOCK_COLLECTION);
        assert_eq!(block["record"]["subject"], target);

        let deletes: Vec<serde_json::Value> = requests
            .iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.deleteRecord")
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect();
        assert_eq!(deletes.len(), 4, "two follows and two blocks: {deletes:?}");
        for rkey in ["oldfollow", "oldblock"] {
            assert!(
                deletes.iter().any(|body| body["rkey"] == rkey),
                "{rkey} was deleted: {deletes:?}"
            );
        }
        assert_eq!(
            deletes
                .iter()
                .filter(|body| body["collection"] == graph::FOLLOW_COLLECTION)
                .count(),
            2
        );
        assert_eq!(
            deletes
                .iter()
                .filter(|body| body["collection"] == graph::BLOCK_COLLECTION)
                .count(),
            2
        );

        let mutes: Vec<_> = requests
            .iter()
            .filter(|r| r.path.starts_with("/xrpc/app.bsky.graph."))
            .collect();
        assert_eq!(mutes.len(), 2, "muteActor + unmuteActor");
        assert!(mutes.iter().all(|r| r.authorized));
        assert!(
            mutes
                .iter()
                .any(|r| r.path == "/xrpc/app.bsky.graph.muteActor")
        );
        assert!(
            mutes
                .iter()
                .any(|r| r.path == "/xrpc/app.bsky.graph.unmuteActor")
        );

        // Undoing a relationship that is not there fails with the reason.
        command_tx
            .send(Command::GraphWrite {
                action: "unfollow".into(),
                subject: target.into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("not following"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        // A self-action is refused before any HTTP call.
        let before = pds.non_poll_requests().len();
        command_tx
            .send(Command::GraphWrite {
                action: "follow".into(),
                subject: "did:plc:stubuser".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => {
                assert!(message.contains("your own account"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            pds.non_poll_requests().len(),
            before,
            "a self-action does not touch the network"
        );

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn interactions_resolve_the_target_and_publish_records() {
        let pds = StubPds::start(false);
        let dir = temp_dir("interactions");
        write_config(&dir, &test_config(pds.url.clone()));
        let (command_tx, event_rx, handle) =
            spawn_worker_with_resolver(dir.clone(), stub_resolver(&pds.url));

        command_tx.send(Command::Login).unwrap();
        let _ = wait_for(&event_rx, |e| {
            matches!(e, Event::Session(SessionState::LoggedIn { .. }))
        });

        // A target post that is itself a reply in a thread.
        pds.seed_record(
            interactions::POST_COLLECTION,
            "target1",
            serde_json::json!({
                "$type": interactions::POST_COLLECTION,
                "text": "parent post",
                "reply": {
                    "root": {"uri": "at://did:plc:root/app.bsky.feed.post/root1", "cid": STUB_CIDS[1]},
                    "parent": {"uri": "at://did:plc:root/app.bsky.feed.post/parent1", "cid": STUB_CIDS[1]},
                },
            }),
        );
        let target = "at://did:plc:target/app.bsky.feed.post/target1";

        for command in [
            Command::Like { uri: target.into() },
            Command::Repost { uri: target.into() },
        ] {
            command_tx.send(command).unwrap();
            match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
                Event::Posted { uri } => assert!(uri.starts_with("at://did:plc:stubuser/")),
                other => panic!("expected Posted, got {other:?}"),
            }
        }
        command_tx
            .send(Command::Reply {
                uri: target.into(),
                text: "nice thread".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Posted { .. })) {
            Event::Posted { uri } => assert!(uri.contains(interactions::POST_COLLECTION)),
            other => panic!("expected Posted, got {other:?}"),
        }

        // A non-post target fails before any record is written.
        let creates_before = pds
            .requests()
            .iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .count();
        command_tx
            .send(Command::Like {
                uri: "at://did:plc:target/app.bsky.feed.like/1".into(),
            })
            .unwrap();
        match wait_for(&event_rx, |e| matches!(e, Event::Failed { .. })) {
            Event::Failed { message } => assert!(message.contains("want a post"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            pds.requests()
                .iter()
                .filter(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
                .count(),
            creates_before,
            "no record for a non-post target"
        );

        let creates: Vec<serde_json::Value> = pds
            .requests()
            .iter()
            .filter(|r| r.path == "/xrpc/com.atproto.repo.createRecord")
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect();
        let by_collection = |collection: &str| {
            creates
                .iter()
                .find(|body| body["collection"] == collection)
                .unwrap_or_else(|| panic!("no createRecord for {collection}: {creates:?}"))
        };

        let like = by_collection(interactions::LIKE_COLLECTION);
        assert_eq!(like["record"]["$type"], interactions::LIKE_COLLECTION);
        assert_eq!(like["record"]["subject"]["uri"], target);
        assert_eq!(like["record"]["subject"]["cid"], STUB_CIDS[0]);

        let repost = by_collection(interactions::REPOST_COLLECTION);
        assert_eq!(repost["record"]["subject"]["uri"], target);

        let reply = by_collection(interactions::POST_COLLECTION);
        assert_eq!(reply["record"]["text"], "nice thread");
        assert_eq!(reply["record"]["reply"]["parent"]["uri"], target);
        assert_eq!(
            reply["record"]["reply"]["root"]["uri"],
            "at://did:plc:root/app.bsky.feed.post/root1"
        );

        // The target reads were unauthenticated.
        let reads: Vec<_> = pds
            .requests()
            .into_iter()
            .filter(|r| r.path.starts_with("/xrpc/com.atproto.repo.getRecord"))
            .collect();
        assert_eq!(reads.len(), 3, "one per interaction");
        assert!(reads.iter().all(|r| !r.authorized));

        command_tx.send(Command::Shutdown).unwrap();
        handle.join().unwrap();
    }
}
