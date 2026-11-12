//! Manual end-to-end check of the worker against a real PDS.
//!
//! App-password mode — credentials come from the environment and are never
//! written anywhere but the throwaway settings file in the system temp folder:
//!
//! ```sh
//! ATMOSPHERIC_PDS_URL="https://eurosky.social" \
//! ATMOSPHERIC_IDENTIFIER="you.example" \
//! ATMOSPHERIC_APP_PASSWORD="xxxx-xxxx-xxxx-xxxx" \
//! ATMOSPHERIC_POST_TEXT="hello from atmospheric-horizon (M2 check)" \
//! ATMOSPHERIC_POST_IMAGE="/path/to/screenshot.png" \   # optional, M3
//! ATMOSPHERIC_POST_CHAT="hello from in-game chat" \     # optional, M5
//! ATMOSPHERIC_POST_SNAPSHOT=1 \                          # optional, M6
//! cargo run -p atmospheric --example pds_check
//! ```
//!
//! OAuth mode (M8) instead of the app password: set `ATMOSPHERIC_OAUTH_HANDLE`
//! (the PDS URL and app password are then not needed). The worker resolves the
//! handle, prints the authorize URL as an `OAuthAuthorize` event, and you
//! finish the login in the browser that opens:
//!
//! ```sh
//! ATMOSPHERIC_OAUTH_HANDLE="you.example" \
//! ATMOSPHERIC_POST_TEXT="hello over OAuth (M8 check)" \
//! cargo run -p atmospheric --example pds_check
//! ```
//!
//! A second run resumes the persisted OAuth session (no browser needed).
//!
//! `ATMOSPHERIC_OAUTH_PAR_ONLY=1` skips the worker and just resolves the
//! handle, discovers the PDS/authorization server, and pushes the PAR against
//! the real services — a headless wire-format check that exits before the
//! browser step. `ATMOSPHERIC_PROBE_AS=https://some.authorization.server`
//! pushes the PAR at that specific authorization server instead of the
//! account's own (debugging client_id handling).
//!
//! The example spawns the worker exactly as the game side will, sends the
//! login command, waits for the session event, optionally sends the posts,
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
    let oauth_handle = env::var("ATMOSPHERIC_OAUTH_HANDLE").ok();
    if env::var("ATMOSPHERIC_OAUTH_PAR_ONLY").is_ok()
        && let Some(handle) = &oauth_handle
    {
        par_only_check(handle);
        return;
    }
    let config = Config {
        pds_url: env::var("ATMOSPHERIC_PDS_URL").unwrap_or_else(|_| "https://bsky.social".into()),
        identifier: env::var("ATMOSPHERIC_IDENTIFIER").unwrap_or_default(),
        app_password: env::var("ATMOSPHERIC_APP_PASSWORD").unwrap_or_default(),
        poll_interval_secs: 60,
        post_death_messages: false,
        post_chat_messages: false,
        chat_server_id: "pds-check".into(),
        persist_session: true,
        oauth_client_id: env::var("ATMOSPHERIC_OAUTH_CLIENT_ID").unwrap_or_default(),
    };
    if oauth_handle.is_none() {
        // App-password mode needs all three; OAuth mode needs none of them.
        required("ATMOSPHERIC_PDS_URL");
        required("ATMOSPHERIC_IDENTIFIER");
        required("ATMOSPHERIC_APP_PASSWORD");
    }
    let post_text = env::var("ATMOSPHERIC_POST_TEXT").ok();
    let post_image = env::var("ATMOSPHERIC_POST_IMAGE").ok();
    let post_chat = env::var("ATMOSPHERIC_POST_CHAT").ok();
    let post_snapshot = env::var("ATMOSPHERIC_POST_SNAPSHOT").is_ok();

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

    // In OAuth mode a persisted session may already resume at startup; give
    // it a moment before sending a fresh login.
    let mut logged_in = false;
    if let Some(handle_name) = &oauth_handle {
        if let Ok(event) = event_rx.recv_timeout(Duration::from_secs(15)) {
            println!("event: {event:?}");
            if let Event::Session(SessionState::LoggedIn { .. }) = event {
                logged_in = true;
                println!("resumed the persisted OAuth session");
            }
        }
        if !logged_in {
            command_tx
                .send(Command::LoginOAuth {
                    handle: handle_name.clone(),
                })
                .expect("send LoginOAuth");
        }
    } else {
        command_tx.send(Command::Login).expect("send Login");
    }
    while !logged_in {
        let timeout = if oauth_handle.is_some() {
            // The browser step needs a human: wait as long as the worker does.
            Duration::from_secs(360)
        } else {
            Duration::from_secs(60)
        };
        match event_rx.recv_timeout(timeout) {
            Ok(event) => {
                println!("event: {event:?}");
                match event {
                    Event::Session(SessionState::LoggedIn { .. }) => logged_in = true,
                    Event::OAuthAuthorize { url } => {
                        println!("OAUTH AUTHORIZE URL (finish the login in the browser): {url}");
                    }
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
                let _ = command_tx.send(Command::Shutdown);
                let _ = handle.join();
                exit(1);
            }
        }
    }

    // One wait loop per optional post; each ends on its Posted event. (A
    // stray notification event from the poll timer is just printed.)
    let posts: Vec<Command> = [
        post_text.map(|text| Command::PostText { text }),
        post_image.map(|path| Command::PostImage {
            path,
            alt_text: "pds_check image post".into(),
            text: Some("atmospheric-horizon image check (test post, feel free to delete)".into()),
        }),
        post_chat.map(|message| Command::PostChat {
            message,
            automatic: false,
        }),
        post_snapshot.then(|| Command::PostSnapshot {
            summary: atmospheric::records::SnapshotSummary {
                username: String::new(), // the worker fills it from the identifier
                dimension: "minecraft:overworld".into(),
                position: atmospheric::records::Position { x: 0, y: 64, z: 0 },
                health: "20".into(),
                food: 20,
                xp_level: 0,
                game_mode: "survival".into(),
                team: None,
                scores: None,
            },
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    for post in posts {
        let kind = match &post {
            Command::PostText { .. } => "text",
            Command::PostImage { .. } => "image",
            Command::PostChat { .. } => "chat record",
            Command::PostSnapshot { .. } => "snapshot record",
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

    // Poll notifications once at the end (M4), printing the digest event. A
    // failure here is not fatal to the check: some PDSs reject the seenAt
    // hint, and the timer poll may already have delivered the digest.
    command_tx
        .send(Command::RefreshNotifications)
        .expect("send RefreshNotifications");
    loop {
        match event_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(event) => {
                println!("event: {event:?}");
                match event {
                    Event::Notifications { .. } => break,
                    Event::Failed { message } => {
                        eprintln!("notification poll failed (non-fatal): {message}");
                        break;
                    }
                    _ => {}
                }
            }
            Err(error) => {
                eprintln!("no event from the worker: {error}");
                break;
            }
        }
    }

    command_tx.send(Command::Shutdown).expect("send Shutdown");
    handle.join().expect("worker thread joins cleanly");
    println!("worker shut down cleanly");
}

/// Headless wire-format check (M8): resolve the handle, discover the PDS and
/// its authorization server, and push the PAR — all against the real
/// services. Stops before the browser step; exit 0 means the authorization
/// server accepted the pushed request and returned a `request_uri`.
fn par_only_check(handle: &str) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build the runtime");
    runtime.block_on(async {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build the http client");
        let scope = atmospheric::oauth::DEFAULT_SCOPE;
        let client_id = env::var("ATMOSPHERIC_OAUTH_CLIENT_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| atmospheric::oauth::client_id(scope));
        println!("client_id: {client_id}");
        println!("resolving handle {handle} and pushing the PAR (scope: {scope})…");

        let auth_server = if let Ok(probe) = env::var("ATMOSPHERIC_PROBE_AS") {
            // Push the PAR at a specific authorization server instead of the
            // account's own (debugging client_id handling).
            atmospheric::oauth::AuthServer {
                issuer: probe.clone(),
                authorization_endpoint: format!("{probe}/oauth/authorize"),
                token_endpoint: format!("{probe}/oauth/token"),
                par_endpoint: format!("{probe}/oauth/par"),
            }
        } else {
            let resolver = atmospheric::oauth::Resolver::default();
            let (did, pds_url) = atmospheric::oauth::resolve_account(&http, &resolver, handle)
                .await
                .expect("resolve the account");
            println!("did: {did}");
            println!("pds: {pds_url}");
            atmospheric::oauth::discover_auth_server(&http, &pds_url)
                .await
                .expect("discover the authorization server")
        };
        println!("issuer: {}", auth_server.issuer);
        println!("par endpoint: {}", auth_server.par_endpoint);

        let key = atmospheric::oauth::DpopKey::generate();
        let request_uri = atmospheric::oauth::push_par(
            &http,
            &auth_server,
            &client_id,
            scope,
            handle,
            "http://127.0.0.1:9/",
            &key,
        )
        .await
        .expect("push the PAR");
        println!("PAR accepted, request_uri: {request_uri}");

        let authorize_url = format!(
            "{}?request_uri={}&client_id={}",
            auth_server.authorization_endpoint,
            atmospheric::oauth::url_encode(&request_uri),
            atmospheric::oauth::url_encode(&client_id)
        );
        println!("authorize_url: {authorize_url}");
        // The authorize endpoint should accept the request_uri (a login page
        // or a redirect to one); a 400 here means the PAR was rejected.
        let response = http
            .get(&authorize_url)
            .send()
            .await
            .expect("GET the authorize URL");
        let status = response.status();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.unwrap_or_default();
        println!("authorize endpoint answered HTTP {status} location={location:?}");
        if status.is_success() || status.is_redirection() {
            println!("the authorize endpoint accepted the request_uri (login page served)");
        } else {
            // ASes commonly bot-guard the authorize page (e.g. requiring a
            // browser's sec-fetch-site header); that only affects real
            // browsers, not the PAR we already proved was accepted.
            println!(
                "authorize page needs a real browser (HTTP {status}); body starts: {:.80}",
                body
            );
        }
        println!(
            "PAR accepted by the real authorization server — wire format OK up to the browser step"
        );
    });
}
