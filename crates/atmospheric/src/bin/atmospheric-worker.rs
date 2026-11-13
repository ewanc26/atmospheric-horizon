//! Sidecar worker binary for the atmospheric Horizon mod (M17, issue #2
//! option 3).
//!
//! The game-side module (`mixins/atmospheric/runtime.rs`, std-only) spawns
//! this binary next to the game executable and talks to it over stdio:
//! newline-delimited JSON [`Command`]s on stdin in, newline-delimited JSON
//! [`Event`]s on stdout out (serde's externally tagged format — pinned by the
//! crate's unit tests). Stdin EOF shuts the worker down cleanly, so the game
//! process exiting closes the pipe and ends this process.
//!
//! Usage: `atmospheric-worker --data-dir <path> [--no-browser]`
//!
//! - `--data-dir <path>` — the mod's storage folder (or `ATMOSPHERIC_DATA_DIR`);
//! - `--no-browser` — never open the system browser for OAuth logins
//!   (tests, headless runs);
//! - `-h`/`--help` — usage.
//!
//! Diagnostics go to stderr; stdout is protocol only.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::channel;

use atmospheric::worker::{WorkerOptions, spawn};
use atmospheric::{Command, Event};

const USAGE: &str = "usage: atmospheric-worker --data-dir <path> [--no-browser]";

fn main() {
    let mut data_dir: Option<PathBuf> = None;
    let mut no_browser = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => match args.next() {
                Some(value) => data_dir = Some(PathBuf::from(value)),
                None => {
                    eprintln!("--data-dir needs a path\n{USAGE}");
                    std::process::exit(2);
                }
            },
            "--no-browser" => no_browser = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let data_dir =
        match data_dir.or_else(|| std::env::var_os("ATMOSPHERIC_DATA_DIR").map(PathBuf::from)) {
            Some(dir) => dir,
            None => {
                eprintln!("no data dir: pass --data-dir or set ATMOSPHERIC_DATA_DIR");
                std::process::exit(2);
            }
        };
    let mut options = WorkerOptions::new(data_dir);
    if no_browser {
        options = options.no_browser();
    }

    let (command_tx, command_rx) = channel::<Command>();
    let (event_tx, event_rx) = channel::<Event>();
    // Kept aside so unparseable stdin lines can be reported back as events.
    let report_tx = event_tx.clone();
    let _worker = spawn(command_rx, event_tx, options);

    // stdin lines -> commands. EOF (the game exited) drops `command_tx`,
    // which stops the worker loop.
    let reader = std::thread::Builder::new()
        .name("atmospheric-worker-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in BufReader::new(stdin.lock()).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Command>(&line) {
                    Ok(command) => {
                        let shutdown = matches!(command, Command::Shutdown);
                        if command_tx.send(command).is_err() {
                            break;
                        }
                        if shutdown {
                            break;
                        }
                    }
                    Err(error) => {
                        let report = Event::Failed {
                            message: format!("bad command line: {error}"),
                        };
                        if report_tx.send(report).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    let reader = match reader {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("could not start the stdin reader: {error}");
            std::process::exit(1);
        }
    };

    // events -> stdout, one JSON line each, flushed so the game side sees
    // them promptly. When the worker thread ends (Shutdown or EOF) and the
    // reader thread has dropped its senders, this loop ends and the
    // process exits.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for event in event_rx {
        let Ok(line) = serde_json::to_string(&event) else {
            continue;
        };
        let written = out
            .write_all(line.as_bytes())
            .and_then(|_| out.write_all(b"\n"))
            .and_then(|_| out.flush());
        if written.is_err() {
            break; // the game side is gone
        }
    }
    let _ = reader.join();
}
