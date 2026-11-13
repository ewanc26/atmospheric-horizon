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
//! ATMOSPHERIC_POST_SKIN="/path/to/skin.png" \          # optional, M9
//! ATMOSPHERIC_POST_CAPE="/path/to/cape.png" \          # optional, M9
//! ATMOSPHERIC_SKIN_MODEL="slim" \                        # optional, M9
//! ATMOSPHERIC_POST_SERVER_PROFILE=1 \                    # optional, M10
//! ATMOSPHERIC_FETCH_LIST="at://did:…/app.bsky.graph.list/<rkey>" \  # optional, M11
//! ATMOSPHERIC_FETCH_LABELS="at://did:…/app.bsky.feed.post/<rkey>" \ # optional, M12
//! ATMOSPHERIC_LABELER="did:plc:…" \                             # optional, M12
//! ATMOSPHERIC_LABELER_SUBJECT="did:plc:…" \                     # optional, M12
//! ATMOSPHERIC_FETCH_PLAYER="did:plc:… | handle" \               # optional, M13
//! ATMOSPHERIC_TRUSTED_SERVER_KEYS="keyId=publicKeyB64,…" \      # optional, M13
//! ATMOSPHERIC_FETCH_SKIN="did:plc:… | handle" \                 # optional, M14
//! ATMOSPHERIC_GATE="at://…/app.bsky.graph.list/<rkey> | URL" \  # optional, M15
//! ATMOSPHERIC_GATE_SUBJECT="did:plc:… | handle" \               # optional, M15 (default: own DID)
//! ATMOSPHERIC_FETCH_CHAT="did:plc:… | handle" \                 # optional, M16
//! ATMOSPHERIC_FETCH_SERVER="did:plc:… | handle" \               # optional, M19
//! ATMOSPHERIC_GRAPH="follows:did:plc:…" \                      # optional, M20
//! ATMOSPHERIC_LIKE="at://…/app.bsky.feed.post/<rkey>" \        # optional, M21
//! ATMOSPHERIC_REPOST="at://…/app.bsky.feed.post/<rkey>" \      # optional, M21
//! ATMOSPHERIC_REPLY="at://…/app.bsky.feed.post/<rkey>|<text>" \ # optional, M21
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
//! account's own (debugging client_id handling). `ATMOSPHERIC_NO_BROWSER=1`
//! keeps the worker from spawning a browser (headless runs): the authorize
//! URL is still printed via the `OAuthAuthorize` event.
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
        skin_path: String::new(),
        cape_path: String::new(),
        skin_model: "default".into(),
        server_name: env::var("ATMOSPHERIC_SERVER_NAME").unwrap_or_default(),
        server_description: String::new(),
        server_address: String::new(),
        server_website: String::new(),
        server_contact: String::new(),
        server_capabilities: vec!["player-data-sync".into(), "skin-sync".into()],
        game_version: "26.3".into(),
        appview_url: "https://public.api.bsky.app".into(),
        // M13: trusted endorsement keys for presence reads, as
        // "keyId=publicKeyB64" pairs separated by commas (an empty keyId is
        // derived from the public key).
        colibri_bridge_enabled: false,
        colibri_channel_uri: String::new(),
        colibri_appview_url: "https://api.colibri.social".into(),
        colibri_appview_did: "did:web:api.colibri.social".into(),
        colibri_server_events_enabled: true,
        trusted_server_keys: env::var("ATMOSPHERIC_TRUSTED_SERVER_KEYS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|raw| {
                raw.split(',')
                    .filter_map(|pair| {
                        let (key_id, public_key) = pair.split_once('=')?;
                        Some(atmospheric::config::TrustedServerKey {
                            key_id: key_id.trim().to_string(),
                            public_key: public_key.trim().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
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
    let post_skin = env::var("ATMOSPHERIC_POST_SKIN").ok();
    let post_cape = env::var("ATMOSPHERIC_POST_CAPE").ok();
    let skin_model = env::var("ATMOSPHERIC_SKIN_MODEL").unwrap_or_else(|_| "default".into());
    let post_server_profile = env::var("ATMOSPHERIC_POST_SERVER_PROFILE").is_ok();

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
    let mut options = WorkerOptions::new(&storage_dir);
    if env::var_os("ATMOSPHERIC_NO_BROWSER").is_some() {
        options = options.no_browser();
    }
    let handle = spawn(command_rx, event_tx, options);

    // In OAuth mode a persisted session may already resume at startup; give
    // it a moment before sending a fresh login.
    let mut logged_in = false;
    let mut own_did = String::new();
    if let Some(handle_name) = &oauth_handle {
        if let Ok(event) = event_rx.recv_timeout(Duration::from_secs(15)) {
            println!("event: {event:?}");
            if let Event::Session(SessionState::LoggedIn { did }) = event {
                logged_in = true;
                own_did = did;
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
                    Event::Session(SessionState::LoggedIn { did }) => {
                        logged_in = true;
                        own_did = did;
                    }
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
        (post_skin.is_some() || post_cape.is_some()).then_some(Command::PostSkin {
            skin_path: post_skin,
            cape_path: post_cape,
            model: skin_model,
        }),
        post_server_profile.then_some(Command::PostServerProfile),
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
            Command::PostSkin { .. } => "skin record",
            Command::PostServerProfile => "server profile",
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

    // Optionally fetch a public list (M11, join-gate read side).
    if let Some(reference) = env::var("ATMOSPHERIC_FETCH_LIST")
        .ok()
        .filter(|s| !s.is_empty())
    {
        command_tx
            .send(Command::FetchList { reference })
            .expect("send FetchList");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::ListMembers { members, .. } => {
                            println!("list has {} members", members.len());
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("list fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally read labels on a subject (M12, labeler read side (a)): the
    // AppView view carries them.
    if let Some(subject) = env::var("ATMOSPHERIC_FETCH_LABELS")
        .ok()
        .filter(|s| !s.is_empty())
    {
        command_tx
            .send(Command::FetchLabels { subject })
            .expect("send FetchLabels");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::Labels { labels, .. } => {
                            println!("{} label(s)", labels.len());
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("label fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally query a labeler directly (M12, labeler read side (b)):
    // resolve its #atproto_labeler endpoint and page queryLabels for the
    // subject (default: the logged-in account's own DID).
    if let Some(labeler) = env::var("ATMOSPHERIC_LABELER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        let subject = env::var("ATMOSPHERIC_LABELER_SUBJECT").ok();
        command_tx
            .send(Command::FetchLabelerLabels { labeler, subject })
            .expect("send FetchLabelerLabels");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::Labels { labels, .. } => {
                            println!("labeler reports {} label(s)", labels.len());
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("labeler query failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally read a player's latest snapshot record (M13, presence read
    // side): resolved to their PDS via the DID document, listed
    // unauthenticated, and verified against the own/trusted keys.
    if let Some(subject) = env::var("ATMOSPHERIC_FETCH_PLAYER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        command_tx
            .send(Command::FetchPlayer { subject })
            .expect("send FetchPlayer");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::PlayerSnapshot { snapshot, .. } => {
                            match &*snapshot {
                                Some(d) => println!(
                                    "player snapshot: {} v{} — {:?}",
                                    d.username, d.version, d.verdict
                                ),
                                None => println!("no snapshot records"),
                            }
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("player fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally read a player's latest skin record (M14, skin-sync read
    // side): listed unauthenticated from their own PDS, endorsement-verified,
    // and (when verified) the blobs are downloaded into <storage>/skins/.
    if let Some(subject) = env::var("ATMOSPHERIC_FETCH_SKIN")
        .ok()
        .filter(|s| !s.is_empty())
    {
        command_tx
            .send(Command::FetchPlayerSkin { subject })
            .expect("send FetchPlayerSkin");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::PlayerSkin { skin, .. } => {
                            match &*skin {
                                Some(d) => println!(
                                    "skin record: model {}, verdict {:?}, skin {:?}",
                                    d.model,
                                    d.verdict,
                                    d.skin.as_ref().map(|b| b.path.clone())
                                ),
                                None => println!("no skin records"),
                            }
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("skin fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally run the join-gate decision (M15): is the subject (default:
    // the logged-in account's own DID) on the list? Fail-closed — a fetch
    // error exits non-zero as the deny-with-reason.
    if let Some(list) = env::var("ATMOSPHERIC_GATE").ok().filter(|s| !s.is_empty()) {
        let subject = env::var("ATMOSPHERIC_GATE_SUBJECT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| own_did.clone());
        command_tx
            .send(Command::CheckGate { list, subject })
            .expect("send CheckGate");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::GateDecision { allowed, .. } => {
                            println!("gate decision: allowed={allowed}");
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("gate check failed (deny-with-reason): {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally read a player's chat history (M16, read side of the M5 chat
    // mirror): unauthenticated listRecords on their own PDS, ordered by
    // sentAt. Chat records are unsigned by design — no verdict.
    if let Some(subject) = env::var("ATMOSPHERIC_FETCH_CHAT")
        .ok()
        .filter(|s| !s.is_empty())
    {
        command_tx
            .send(Command::FetchChat { subject })
            .expect("send FetchChat");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::ChatHistory { total, items, .. } => {
                            println!("chat history: {total} record(s), newest: {items:?}");
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("chat fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally read an account's server profile (M19, read side of M10):
    // unauthenticated listRecords on their own PDS, preferred "self" rkey,
    // and verify the signingKey against trusted keys.
    if let Some(subject) = env::var("ATMOSPHERIC_FETCH_SERVER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        command_tx
            .send(Command::FetchServerProfile { subject })
            .expect("send FetchServerProfile");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::ServerProfile { profile, .. } => {
                            println!("server profile: {profile:?}");
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("server profile fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally read a social graph (M20): public AppView reads of follows,
    // followers, blocks or mutes. Format: "<kind>:<did|handle>".
    if let Some(raw) = env::var("ATMOSPHERIC_GRAPH")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let (kind, subject) = raw
            .split_once(':')
            .expect("ATMOSPHERIC_GRAPH is <kind>:<subject>");
        command_tx
            .send(Command::FetchGraph {
                kind: kind.into(),
                subject: subject.into(),
            })
            .expect("send FetchGraph");
        loop {
            match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => {
                    println!("event: {event:?}");
                    match event {
                        Event::SocialGraph { members, .. } => {
                            println!("social graph: {members:?}");
                            break;
                        }
                        Event::Failed { message } => {
                            eprintln!("social graph fetch failed: {message}");
                            exit(1);
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    eprintln!("no event from the worker: {error}");
                    exit(1);
                }
            }
        }
    }

    // Optionally publish interactions (M21): like, repost and reply to a post.
    if let Some(uri) = env::var("ATMOSPHERIC_LIKE").ok().filter(|s| !s.is_empty()) {
        run_post(&command_tx, &event_rx, Command::Like { uri }, "like");
    }
    if let Some(uri) = env::var("ATMOSPHERIC_REPOST")
        .ok()
        .filter(|s| !s.is_empty())
    {
        run_post(&command_tx, &event_rx, Command::Repost { uri }, "repost");
    }
    if let Some(raw) = env::var("ATMOSPHERIC_REPLY").ok().filter(|s| !s.is_empty()) {
        let (uri, text) = raw
            .split_once('|')
            .expect("ATMOSPHERIC_REPLY is <at-uri>|<text>");
        run_post(
            &command_tx,
            &event_rx,
            Command::Reply {
                uri: uri.into(),
                text: text.into(),
            },
            "reply",
        );
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

/// Send one interaction command and wait for its `Posted` or `Failed` event.
fn run_post(
    command_tx: &mpsc::Sender<Command>,
    event_rx: &mpsc::Receiver<Event>,
    command: Command,
    label: &str,
) {
    command_tx.send(command).expect("send interaction command");
    loop {
        match event_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(event) => {
                println!("event: {event:?}");
                match event {
                    Event::Posted { uri } => {
                        println!("{label}: {uri}");
                        return;
                    }
                    Event::Failed { message } => {
                        eprintln!("{label} failed: {message}");
                        exit(1);
                    }
                    _ => {}
                }
            }
            Err(error) => {
                eprintln!("no event from the worker: {error}");
                exit(1);
            }
        }
    }
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
