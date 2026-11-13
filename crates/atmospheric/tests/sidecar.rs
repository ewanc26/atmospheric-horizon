//! End-to-end tests of the sidecar worker binary (M17, issue #40): spawn the
//! real `atmospheric-worker` bin and talk newline-delimited JSON to it over
//! stdio, exactly like the game-side module does.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "atmospheric-sidecar-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Sidecar {
    child: Arc<Mutex<Child>>,
    reader: BufReader<ChildStdout>,
    stdin: ChildStdin,
}

fn spawn_worker(data_dir: &Path) -> Sidecar {
    let mut child = Command::new(env!("CARGO_BIN_EXE_atmospheric-worker"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--no-browser")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the sidecar worker binary");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let child = Arc::new(Mutex::new(child));
    // Watchdog: if the worker misbehaves the test must fail, not hang CI.
    let guard = Arc::clone(&child);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        if let Ok(mut child) = guard.lock() {
            let _ = child.kill();
        }
    });
    Sidecar {
        child,
        reader: BufReader::new(stdout),
        stdin,
    }
}

impl Sidecar {
    fn event(&mut self) -> serde_json::Value {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).expect("read event line");
        assert!(read > 0, "worker closed stdout unexpectedly: {line}");
        serde_json::from_str(&line).expect("event line is one JSON value")
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn send(&mut self, command: &atmospheric::Command) {
        self.send_raw(&serde_json::to_string(command).unwrap());
    }

    fn finish(mut self) {
        self.send(&atmospheric::Command::Shutdown);
        drop(self.stdin);
        // The worker stops and closes stdout.
        let mut rest = String::new();
        self.reader
            .read_to_string(&mut rest)
            .expect("drain worker stdout");
        assert!(rest.trim().is_empty(), "unexpected trailing events: {rest}");
        let mut child = self.child.lock().unwrap();
        let status = child.wait().expect("wait for the worker");
        assert!(status.success(), "worker exited with {status}");
    }
}

#[test]
fn handshake_login_bad_line_then_shutdown() {
    let dir = temp_dir("basic");
    let mut worker = spawn_worker(&dir);

    // Handshake: the worker reports its session state on start.
    assert_eq!(worker.event(), serde_json::json!({"Session": "LoggedOut"}));

    // Login with no settings file: LoggingIn, then Failed — no network call.
    worker.send(&atmospheric::Command::Login);
    assert_eq!(worker.event(), serde_json::json!({"Session": "LoggingIn"}));
    let failed = worker.event();
    let message = failed["Session"]["Failed"]["message"]
        .as_str()
        .expect("a Failed session event");
    assert!(
        message.contains("settings file") || message.contains("not configured"),
        "unexpected failure message: {message}"
    );

    // A garbage line comes back as a Failed event, and the worker lives on.
    worker.send_raw("this is not json");
    let bad = worker.event();
    assert!(
        bad["Failed"]["message"]
            .as_str()
            .unwrap()
            .contains("bad command line"),
        "unexpected event: {bad}"
    );

    // PostText while logged out fails fast with "not logged in".
    worker.send(&atmospheric::Command::PostText {
        text: "hello".into(),
    });
    let failed = worker.event();
    assert!(
        failed["Failed"]["message"]
            .as_str()
            .unwrap()
            .contains("not logged in"),
        "unexpected event: {failed}"
    );

    worker.finish();
}

#[test]
fn save_config_writes_the_settings_file() {
    let dir = temp_dir("save");
    let mut worker = spawn_worker(&dir);
    let _handshake = worker.event();

    worker.send(&atmospheric::Command::SaveConfig {
        config: atmospheric::Config {
            pds_url: "https://pds.example".into(),
            identifier: "alice.example".into(),
            app_password: "aaaa-bbbb-cccc-dddd".into(),
            post_chat_messages: true,
            ..atmospheric::Config::default()
        },
    });
    // SaveConfig is silent on success; poll for the file.
    let config_path = dir.join("config.json");
    let mut written = None;
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(&config_path) {
            written = Some(text);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = written.expect("the worker wrote config.json");
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["pds_url"], "https://pds.example");
    assert_eq!(parsed["identifier"], "alice.example");
    assert_eq!(parsed["post_chat_messages"], true);

    worker.finish();
}

#[test]
fn missing_data_dir_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_atmospheric-worker"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env_remove("ATMOSPHERIC_DATA_DIR")
        .output()
        .expect("run the worker without a data dir");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no data dir"), "stderr: {stderr}");
}
