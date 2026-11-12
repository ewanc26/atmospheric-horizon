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
    session::{self, Agent, SessionState},
};
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
};

/// The settings file's name inside the mod's storage folder.
pub const CONFIG_FILE: &str = "config.json";

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
                while let Some(command) = rx.recv().await {
                    let shutdown = matches!(command, Command::Shutdown);
                    worker.handle(command).await;
                    if shutdown {
                        break;
                    }
                }
                // TODO(M4): a poll interval (`config.poll_interval_secs`)
                // ticks `Command::RefreshNotifications` here, selected
                // against `rx` with `tokio::select!`.
            });
        })
        .expect("spawn the atmospheric worker thread")
}

/// The worker's state: the agent and DID once logged in, plus its files.
struct Worker {
    storage_dir: PathBuf,
    agent: Option<Agent>,
    did: Option<String>,
    events: Sender<Event>,
}

impl Worker {
    fn new(options: WorkerOptions, events: Sender<Event>) -> Self {
        Self {
            storage_dir: options.storage_dir,
            agent: None,
            did: None,
            events,
        }
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
            Command::PostImage { .. } => {
                // TODO(M3): actions::post_image is still `todo!` — answer
                // instead of panicking the worker thread.
                self.fail("image posting lands in M3");
            }
            Command::RefreshNotifications => {
                // TODO(M4): notifications::poll with the persisted cursor.
                self.fail("notification polling lands in M4");
            }
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
        match session::login(&config).await {
            Ok(agent) => match agent.api.com.atproto.server.get_session().await {
                Ok(session) => {
                    let did = session.did.as_str().to_owned();
                    self.agent = Some(agent);
                    self.did = Some(did.clone());
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
        }
    }

    fn write_config(dir: &std::path::Path, config: &Config) {
        std::fs::write(dir.join(CONFIG_FILE), config.to_json().unwrap()).unwrap();
    }

    /// One captured HTTP request: the path and the JSON body.
    #[derive(Debug, Clone)]
    struct Captured {
        path: String,
        body: String,
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
                            handle_connection(&mut stream, &requests, accept_bad_password);
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
            authorized,
        });

        let (status, payload) = match path.as_str() {
            "/xrpc/com.atproto.server.createSession" => {
                if accept_bad_password || !body_string.contains("correct-horse") {
                    (
                        "401 Unauthorized",
                        r#"{"error":"AuthenticationRequired","message":"Invalid identifier or password"}"#,
                    )
                } else {
                    (
                        "200 OK",
                        r#"{"did":"did:plc:stubuser","handle":"stub.test","accessJwt":"stub-access","refreshJwt":"stub-refresh","active":true}"#,
                    )
                }
            }
            "/xrpc/com.atproto.server.getSession" => {
                if authorized {
                    (
                        "200 OK",
                        r#"{"did":"did:plc:stubuser","handle":"stub.test","active":true}"#,
                    )
                } else {
                    (
                        "401 Unauthorized",
                        r#"{"error":"ExpiredToken","message":"no"}"#,
                    )
                }
            }
            "/xrpc/com.atproto.repo.createRecord" => {
                if authorized {
                    (
                        "200 OK",
                        r#"{"uri":"at://did:plc:stubuser/app.bsky.feed.post/3jub4stub","cid":"bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}"#,
                    )
                } else {
                    (
                        "401 Unauthorized",
                        r#"{"error":"ExpiredToken","message":"no"}"#,
                    )
                }
            }
            _ => ("404 Not Found", r#"{"error":"UnknownEndpoint"}"#),
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
