//! Manual end-to-end check of the worker against a real PDS.
//!
//! Credentials come from the environment and are never written anywhere but
//! the throwaway settings file in the system temp folder:
//!
//! ```sh
//! ATMOSPHERIC_PDS_URL="https://bsky.social" \
//! ATMOSPHERIC_IDENTIFIER="you.bsky.social" \
//! ATMOSPHERIC_APP_PASSWORD="xxxx-xxxx-xxxx-xxxx" \
//! ATMOSPHERIC_POST_TEXT="hello from atmospheric-horizon (M2 check)" \
//! cargo run -p atmospheric --example pds_check
//! ```
//!
//! It spawns the worker exactly as the game side will, sends `Login`, waits
//! for the session event, optionally sends `PostText`, prints every event,
//! and shuts the worker down. Exit status is 0 only if login (and the post,
//! when `ATMOSPHERIC_POST_TEXT` is set) succeeded.

use atmospheric::{
    Command, Config, Event,
    session::SessionState,
    worker::{CONFIG_FILE, WorkerOptions, spawn},
};
use std::{env, path::PathBuf, process::exit, sync::mpsc, time::Duration};

fn required(name: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("missing {name} (set it in the environment; see the example's docs)");
            exit(2);
        }
    }
}

fn main() {
    let config = Config {
        pds_url: required("ATMOSPHERIC_PDS_URL"),
        identifier: required("ATMOSPHERIC_IDENTIFIER"),
        app_password: required("ATMOSPHERIC_APP_PASSWORD"),
        poll_interval_secs: 60,
        post_death_messages: false,
    };
    let post_text = env::var("ATMOSPHERIC_POST_TEXT").ok();

    // A throwaway settings folder: the worker reads its config from here.
    let storage_dir: PathBuf = env::temp_dir().join("atmospheric-pds-check");
    std::fs::create_dir_all(&storage_dir).expect("create the temp storage folder");
    std::fs::write(
        storage_dir.join(CONFIG_FILE),
        config.to_json().expect("serialise config"),
    )
    .expect("write the temp settings file");
    println!("settings file: {}", storage_dir.join(CONFIG_FILE).display());

    let (command_tx, command_rx) = mpsc::channel::<Command>();
    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let handle = spawn(command_rx, event_tx, WorkerOptions::new(&storage_dir));

    command_tx.send(Command::Login).expect("send Login");
    let mut logged_in = false;
    while !logged_in {
        match event_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(event) => {
                println!("event: {event:?}");
                match event {
                    Event::Session(SessionState::LoggedIn { .. }) => logged_in = true,
                    Event::Session(SessionState::Failed { message }) => {
                        eprintln!("login failed: {message}");
                        let _ = command_tx.send(Command::Shutdown);
                        let _ = handle.join();
                        exit(1);
                    }
                    _ => {}
                }
            }
            Err(error) => {
                eprintln!("no event from the worker: {error}");
                let _ = handle.join();
                exit(1);
            }
        }
    }

    if let Some(text) = post_text {
        command_tx
            .send(Command::PostText { text })
            .expect("send PostText");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::Posted { .. } => break,
                        Event::Failed { message } => {
                            eprintln!("post failed: {message}");
                            let _ = command_tx.send(Command::Shutdown);
                            let _ = handle.join();
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    let _ = handle.join();
                    exit(1);
                }
            }
        }
    }

    command_tx.send(Command::Shutdown).expect("send Shutdown");
    handle.join().expect("worker thread joins cleanly");
    println!("worker shut down cleanly");
}
