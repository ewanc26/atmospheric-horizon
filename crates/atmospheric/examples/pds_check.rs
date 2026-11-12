//! Manual end-to-end check of the worker against a real PDS.
//!
//! Credentials come from the environment and are never written anywhere but
//! the throwaway settings file in the system temp folder:
//!
//! ```sh
//! ATMOSPHERIC_PDS_URL="https://eurosky.social" \
//! ATMOSPHERIC_IDENTIFIER="you.example" \
//! ATMOSPHERIC_APP_PASSWORD="xxxx-xxxx-xxxx-xxxx" \
//! ATMOSPHERIC_POST_TEXT="hello from atmospheric-horizon (M2 check)" \
//! ATMOSPHERIC_POST_IMAGE="/path/to/screenshot.png" \   # optional, M3
//! cargo run -p atmospheric --example pds_check
//! ```
//!
//! It spawns the worker exactly as the game side will, sends `Login`, waits
//! for the session event, optionally sends `PostText` and/or `PostImage`,
//! prints every event, and shuts the worker down. Exit status is 0 only if
//! login (and the posts, when the env vars are set) succeeded. The PDS URL
//! must be the account's own PDS (resolve the handle's DID to find it), not
//! always bsky.social.

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
    let post_image = env::var("ATMOSPHERIC_POST_IMAGE").ok();

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

    // One wait loop per optional post; each ends on its Posted event.
    let posts: Vec<Command> = [
        post_text.map(|text| Command::PostText { text }),
        post_image.map(|path| Command::PostImage {
            path,
            alt_text: "pds_check image post".into(),
            text: Some("atmospheric-horizon image check (test post, feel free to delete)".into()),
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    for post in posts {
        let kind = match &post {
            Command::PostText { .. } => "text",
            Command::PostImage { .. } => "image",
            _ => unreachable!(),
        };
        command_tx.send(post).expect("send post");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::Posted { .. } => break,
                        Event::Failed { message } => {
                            eprintln!("{kind} post failed: {message}");
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
