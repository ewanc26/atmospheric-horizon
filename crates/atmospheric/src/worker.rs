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
    session::{self, Agent, SessionState},
    tid::TidGen,
};
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    time::Duration,
};

/// The settings file's name inside the mod's storage folder.
pub const CONFIG_FILE: &str = "config.json";
/// The notification cursor's file, same folder.
pub const CURSOR_FILE: &str = "cursor.json";

/// What the worker needs besides the channels: where its files live.
#[derive(Debug, Clone)]
pub struct WorkerOptions {
    /// The mod's own storage folder (Horizon: `native_data_dir("atmospheric")`,
    /// i.e. `horizon-mods/.horizon-mod-data/atmospheric/`). Never a world save.
    pub storage_dir: PathBuf,
}

impl WorkerOptions {
    pub fn new(storage_dir: impl Into<PathBuf>) -> Self {
        Self {
            storage_dir: storage_dir.into(),
        }
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

                let mut worker = Worker::new(options, events);
                worker.emit(Event::Session(SessionState::LoggedOut));
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
                        _ = poll.tick() => {
                            worker.poll_notifications(false).await;
                        }
                    }
                }
            });
        })
        .expect("spawn the atmospheric worker thread")
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
    /// `chat_server_id` from the settings file, refreshed at login.
    chat_server_id: String,
    events: Sender<Event>,
}

impl Worker {
    fn new(options: WorkerOptions, events: Sender<Event>) -> Self {
        // The seen-cursor persists across restarts, so old notifications are
        // not re-shown; a missing file just means "everything is new".
        let cursor = std::fs::read_to_string(options.storage_dir.join(CURSOR_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            storage_dir: options.storage_dir,
            agent: None,
            did: None,
            cursor,
            poll_interval_secs: Config::default().poll_interval_secs,
            tid: TidGen::new(),
            post_chat_messages: false,
            chat_server_id: Config::default().chat_server_id,
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
    fn save_config(&self, config: &Config) -> crate::error::Result<()> {
        std::fs::create_dir_all(&self.storage_dir)?;
        std::fs::write(self.config_path(), config.to_json()?)?;
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
            Command::PostText { text } => {
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
            } => match self.post_image(&path, &alt_text, text.as_deref()).await {
                Ok(uri) => self.emit(Event::Posted { uri }),
                Err(error) => self.fail(error.to_string()),
            },
            Command::RefreshNotifications => self.poll_notifications(true).await,
            Command::PostChat { message, automatic } => {
                // Hook-originated messages only publish when the settings say
                // so; the explicit `/atmospheric chat` command always does.
                if automatic && !self.post_chat_messages {
                    return;
                }
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
        match session::login(&config).await {
            Ok(agent) => match agent.api.com.atproto.server.get_session().await {
                Ok(session) => {
                    let did = session.did.as_str().to_owned();
                    self.agent = Some(agent);
                    self.did = Some(did.clone());
                    self.poll_interval_secs = config.poll_interval_secs.max(5);
                    self.post_chat_messages = config.post_chat_messages;
                    self.chat_server_id = config.chat_server_id.clone();
                    self.emit(Event::Session(SessionState::LoggedIn { did }));
                }
                Err(error) => self.emit(Event::Session(SessionState::Failed {
                    message: AtmosphericError::protocol(error).to_string(),
                })),
            },
            Err(error) => self.emit(Event::Session(SessionState::Failed {
                message: AtmosphericError::protocol(error).to_string(),
            })),
        }
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

    /// A stub PDS on loopback: createSession, getSession and createRecord,
    /// with every request recorded for the assertions.
    struct StubPds {
        url: String,
        requests: Arc<Mutex<Vec<Captured>>>,
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
            let shutdown = Arc::new(Mutex::new(false));
            {
                let requests = Arc::clone(&requests);
                let shutdown = Arc::clone(&shutdown);
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if *shutdown.lock().unwrap() {
                            break;
                        }
                        let Ok(mut stream) = stream else { break };
                        let requests = Arc::clone(&requests);
                        std::thread::spawn(move || {
                            handle_connection(
                                &mut stream,
                                &requests,
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
                shutdown,
            }
        }

        fn requests(&self) -> Vec<Captured> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for StubPds {
        fn drop(&mut self) {
            *self.shutdown.lock().unwrap() = true;
        }
    }

    /// Read one HTTP request and answer it like a PDS would.
    fn handle_connection(
        stream: &mut std::net::TcpStream,
        requests: &Arc<Mutex<Vec<Captured>>>,
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
        for line in lines {
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
            if lower.starts_with("authorization:") && lower.contains("bearer stub-access") {
                authorized = true;
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
                        (
                            "200 OK",
                            format!(
                                r#"{{"blob":{{"$type":"blob","ref":{{"$link":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}},"mimeType":"image/png","size":{size}}}}}"#
                            ),
                        )
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                "/xrpc/com.atproto.repo.createRecord" => {
                    if authorized {
                        (
                        "200 OK",
                        r#"{"uri":"at://did:plc:stubuser/app.bsky.feed.post/3jub4stub","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}"#.to_string(),
                    )
                    } else {
                        (
                            "401 Unauthorized",
                            r#"{"error":"ExpiredToken","message":"no"}"#.to_string(),
                        )
                    }
                }
                _ => (
                    "404 Not Found",
                    r#"{"error":"UnknownEndpoint"}"#.to_string(),
                ),
            }
        };
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

    fn spawn_worker(
        storage_dir: PathBuf,
    ) -> (
        mpsc::Sender<Command>,
        mpsc::Receiver<Event>,
        std::thread::JoinHandle<()>,
    ) {
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        let handle = spawn(command_rx, event_tx, WorkerOptions::new(storage_dir));
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
                assert_eq!(uri, "at://did:plc:stubuser/app.bsky.feed.post/3jub4stub");
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
}
