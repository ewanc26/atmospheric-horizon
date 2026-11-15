// /atmospheric login [handle] logs in (app password from the settings, or
// localhost OAuth when a handle is given); /atmospheric post <text> posts
// text; /atmospheric notifications polls now (the worker also polls on a
// timer); /atmospheric chat <text> publishes one chat record (atpcraft port);
// /atmospheric skin [path] [default|slim] publishes a skin record (M9);
// /atmospheric skin upload opens a browser upload link for the linked player
// (M64);
// /atmospheric list <reference> reads a public list (M11); /atmospheric gate
// <list> <did|handle> decides join-gate membership, fail-closed (M15);
// /atmospheric labels
// <subject> reads labels off a subject's AppView view and /atmospheric labeler
// <did> [subject] queries a labeler directly (M12); /atmospheric player
// <did|handle> reads a player's latest verified snapshot record (M13);
// /atmospheric player-data capture|restore [player] queues aspect capture or a
// verified restore for the next server tick, and /atmospheric rpg discover
// [player] scans statistics and renamed stacks (M63, linked self only);
// /atmospheric skin-of <did|handle> reads their verified skin record and
// downloads the blobs (M14); /atmospheric chat-of <did|handle> reads their
// mirrored chat history (M16); /atmospheric server-of <did|handle> reads their
// server profile record and verifies its signing key (M19); /atmospheric
// server-account login|status|logout|publish manages the separate server
// account (M25); /atmospheric
// profile <did|handle> reads a Bluesky profile with atpcraft's pronoun
// precedence (M24); /atmospheric profile-set <name|description|pronouns>
// [text] updates the logged-in account's profile (M65); /atmospheric
// follows|followers|blocks|mutes <did|handle>
// reads a public social graph (M20);
// follow|unfollow|block|unblock|mute|unmute <did|handle> writes one (M23);
// /atmospheric records <did|handle> <collection> browses any public collection
// (M44); /atmospheric reply <at-uri> <text>, /atmospheric like <at-uri> and
// /atmospheric repost <at-uri> publish interactions (M21); /atmospheric link
// <code> claims a server identity pairing code (M36).
fn atmospheric_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) {
        return;
    }
    let mut parts = command.splitn(2, ' ');
    if parts.next() != Some("atmospheric") {
        return;
    }
    let usage = "atmospheric: try /atmospheric login [handle], /atmospheric post <text>, /atmospheric chat <text>, /atmospheric snapshot, /atmospheric player-data capture|restore [player], /atmospheric rpg discover [player], /atmospheric skin [path] [default|slim], /atmospheric skin upload, /atmospheric server, /atmospheric server-account login <handle>|status|logout|publish, /atmospheric colibri start|stop|reconnect|status|send <text>, /atmospheric link [code], /atmospheric server-of <did|handle>, /atmospheric profile <did|handle>, /atmospheric profile-set <name|description|pronouns> [text], /atmospheric follows|followers|blocks|mutes <did|handle>, /atmospheric follow|unfollow|block|unblock|mute|unmute <did|handle>, /atmospheric records <did|handle> <collection>, /atmospheric reply <at-uri> <text>, /atmospheric like <at-uri>, /atmospheric repost <at-uri>, /atmospheric list <list>, /atmospheric gate <list> <did|handle>, /atmospheric labels <subject>, /atmospheric labeler <did> [subject], /atmospheric labeler-identity [status|request|submit <token>], /atmospheric player <did|handle>, /atmospheric skin-of <did|handle>, /atmospheric chat-of <did|handle>, /atmospheric logout or /atmospheric notifications";
    let Some(rest) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
        *ran = true;
        game.native_chat(usage);
        return;
    };
    if rest == "login" || rest.starts_with("login ") {
        let handle = rest.strip_prefix("login").unwrap_or("").trim();
        *ran = true;
        if handle.is_empty() {
            m::login();
            game.native_chat("atmospheric: logging in with the saved app password…");
        } else {
            m::login_oauth(handle);
            game.native_chat(&format!(
                "atmospheric: starting OAuth login for {handle} — a browser window should open; otherwise use the authorize link shown in chat"
            ));
        }
    } else if rest == "server-account" || rest.starts_with("server-account ") {
        let args: Vec<&str> = rest
            .strip_prefix("server-account")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        match args.as_slice() {
            ["login"] => game.native_chat(
                "atmospheric: /atmospheric server-account login <handle>",
            ),
            ["login", handle] => {
                m::login_server_oauth(handle);
                game.native_chat(&format!(
                    "atmospheric: starting server-account OAuth login for {handle} — a browser window should open"
                ));
            }
            ["status"] => {
                m::server_account_status();
                game.native_chat("atmospheric: checking the server account…");
            }
            ["logout"] => {
                m::logout_server_account();
                game.native_chat("atmospheric: logging out the server account…");
            }
            ["publish"] => {
                m::post_server_profile_as_server();
                game.native_chat("atmospheric: publishing the server profile from the server account…");
            }
            _ => game.native_chat(
                "atmospheric: /atmospheric server-account login <handle>|status|logout|publish",
            ),
        }
    } else if rest == "colibri" || rest.starts_with("colibri ") {
        let payload = rest.strip_prefix("colibri").unwrap_or("").trim();
        *ran = true;
        if payload == "start" {
            m::colibri_start();
            game.native_chat("atmospheric: starting the Colibri bridge…");
        } else if payload == "stop" {
            m::colibri_stop();
            game.native_chat("atmospheric: stopping the Colibri bridge…");
        } else if payload == "reconnect" {
            m::colibri_reconnect();
            game.native_chat("atmospheric: reconnecting the Colibri stream…");
        } else if payload == "status" {
            m::colibri_status();
            game.native_chat("atmospheric: checking the Colibri bridge…");
        } else if let Some(text) = payload.strip_prefix("send ") {
            if text.trim().is_empty() {
                game.native_chat("atmospheric: /atmospheric colibri send <text>");
            } else {
                m::colibri_send(text.trim());
                game.native_chat("atmospheric: sending to Colibri…");
            }
        } else {
            game.native_chat(
                "atmospheric: /atmospheric colibri start|stop|reconnect|status|send <text>",
            );
        }
    } else if rest == "link" || rest.starts_with("link ") {
        let code = rest.strip_prefix("link").unwrap_or("").trim();
        *ran = true;
        if code.is_empty() {
            match game.native_acting_player() {
                Some(player) => {
                    m::start_browser_link(m::uuid_text(player.uuid), &player.name);
                    game.native_chat(
                        "atmospheric: starting browser linking — the one-time URL will appear in the HUD; open it and sign in with Bluesky",
                    );
                }
                None => game.native_chat(
                    "atmospheric: run /atmospheric link as a player on a server, or use /atmospheric link <pairing-code>",
                ),
            }
        } else {
            m::claim_server_link(code);
            game.native_chat("atmospheric: claiming the server identity link…");
        }
    } else if rest == "notifications" {
        *ran = true;
        m::refresh_notifications();
        game.native_chat("atmospheric: checking notifications…");
    } else if rest == "post" || rest.starts_with("post ") {
        let text = rest.strip_prefix("post").unwrap_or("").trim();
        *ran = true;
        if text.is_empty() {
            game.native_chat("atmospheric: /atmospheric post <text>");
        } else {
            m::post_text(text);
            game.native_chat("atmospheric: posting…");
        }
    } else if rest == "logout" {
        *ran = true;
        m::logout();
        game.native_chat("atmospheric: logging out…");
    } else if rest == "snapshot" {
        *ran = true;
        match game.native_player() {
            Some(p) => {
                m::post_snapshot(m::snapshot_from_player("", p.dimension, p.x, p.y, p.z));
                game.native_chat("atmospheric: publishing the snapshot record…");
            }
            None => game.native_chat("atmospheric: no player in this world"),
        }
    } else if rest == "player-data" || rest.starts_with("player-data ") {
        let args: Vec<&str> = rest
            .strip_prefix("player-data")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        let action = args.first().copied().unwrap_or_default();
        let requested = args.get(1).copied();
        let players = game.native_server_players();
        let target = requested
            .and_then(|name| {
                players
                    .iter()
                    .find(|player| player.name.eq_ignore_ascii_case(name))
            })
            .cloned()
            .or_else(|| game.native_acting_player());
        match (action, target) {
            ("capture", Some(player)) => {
                m::queue_player_capture(&m::uuid_text(player.uuid), &player.name);
                game.native_tell(
                    player.uuid,
                    horizon_client::text::Text::literal(
                        "atmospheric: capturing your player data on the next tick…",
                    ),
                );
            }
            ("restore", Some(player)) => {
                m::fetch_player_data(&m::uuid_text(player.uuid), &player.name);
                game.native_tell(
                    player.uuid,
                    horizon_client::text::Text::literal(
                        "atmospheric: fetching your latest verified snapshot…",
                    ),
                );
            }
            ("capture" | "restore", None) => game.native_chat(
                "atmospheric: /atmospheric player-data capture|restore <player> — run it on a server with the player online",
            ),
            _ => game.native_chat(
                "atmospheric: /atmospheric player-data capture|restore [player]",
            ),
        }
    } else if rest == "rpg" || rest.starts_with("rpg ") {
        let args: Vec<&str> = rest
            .strip_prefix("rpg")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        if args.first() != Some(&"discover") {
            game.native_chat("atmospheric: /atmospheric rpg discover [player]");
        } else {
            let requested = args.get(1).copied();
            let players = game.native_server_players();
            let target = requested
                .and_then(|name| {
                    players
                        .iter()
                        .find(|player| player.name.eq_ignore_ascii_case(name))
                })
                .cloned()
                .or_else(|| game.native_acting_player());
            match target {
                Some(player) => {
                    m::queue_rpg_scan(&m::uuid_text(player.uuid), &player.name);
                    game.native_tell(
                        player.uuid,
                        horizon_client::text::Text::literal(
                            "atmospheric: scanning your item statistics and inventory for RPG discoveries…",
                        ),
                    );
                }
                None => game.native_chat(
                    "atmospheric: /atmospheric rpg discover <player> — run it on a server with the player online",
                ),
            }
        }
    } else if rest == "skin upload" {
        *ran = true;
        match game.native_acting_player() {
            Some(player) => {
                m::start_skin_upload(m::uuid_text(player.uuid), &player.name);
                game.native_chat(
                    "atmospheric: creating a one-time browser skin-upload link for you…",
                );
            }
            None => game.native_chat(
                "atmospheric: /atmospheric skin upload — run it as the linked player",
            ),
        }
    } else if rest == "skin" || rest.starts_with("skin ") {
        let args: Vec<&str> = rest
            .strip_prefix("skin")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        let skin_path = args.first().map(|s| s.to_string());
        let model = args.get(1).copied().unwrap_or("default");
        if args.len() > 2 || !["default", "slim"].contains(&model) {
            game.native_chat("atmospheric: /atmospheric skin [path/to/skin.png] [default|slim]");
        } else {
            m::post_skin(skin_path, None, model);
            game.native_chat("atmospheric: publishing the skin record…");
        }
    } else if rest == "server" {
        *ran = true;
        m::post_server_profile();
        game.native_chat("atmospheric: publishing the server profile record…");
    } else if rest == "list" || rest.starts_with("list ") {
        let reference = rest.strip_prefix("list").unwrap_or("").trim();
        *ran = true;
        if reference.is_empty() {
            game.native_chat(
                "atmospheric: /atmospheric list <at://…/app.bsky.graph.list/<rkey> | bsky.app list URL>",
            );
        } else {
            m::fetch_list(reference);
            game.native_chat("atmospheric: fetching the list…");
        }
    } else if rest == "gate" || rest.starts_with("gate ") {
        let args: Vec<&str> = rest
            .strip_prefix("gate")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        match args.as_slice() {
            [list, subject] => {
                m::check_gate(list, subject);
                game.native_chat("atmospheric: checking the join gate…");
            }
            _ => game.native_chat(
                "atmospheric: /atmospheric gate <at://…/app.bsky.graph.list/<rkey> | bsky.app list URL> <did | handle>",
            ),
        }
    } else if rest == "labels" || rest.starts_with("labels ") {
        let subject = rest.strip_prefix("labels").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat(
                "atmospheric: /atmospheric labels <at://… | did | handle | bsky.app URL>",
            );
        } else {
            m::fetch_labels(subject);
            game.native_chat("atmospheric: reading the labels…");
        }
    } else if rest == "labeler" || rest.starts_with("labeler ") {
        let args: Vec<&str> = rest
            .strip_prefix("labeler")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        match args.as_slice() {
            [labeler] => {
                m::fetch_labeler_labels(labeler, None);
                game.native_chat(
                    "atmospheric: querying the labeler for your account's labels…",
                );
            }
            [labeler, subject] => {
                m::fetch_labeler_labels(labeler, Some(subject));
                game.native_chat("atmospheric: querying the labeler…");
            }
            _ => game.native_chat("atmospheric: /atmospheric labeler <labeler-did> [subject]"),
        }
    } else if rest == "labeler-identity" || rest.starts_with("labeler-identity ") {
        let args: Vec<&str> = rest
            .strip_prefix("labeler-identity")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        *ran = true;
        match args.as_slice() {
            [] | ["status"] => {
                m::labeler_identity_status();
                game.native_chat("atmospheric: checking the labeler identity…");
            }
            ["request"] => {
                m::request_labeler_plc_signature();
                game.native_chat("atmospheric: requesting a PLC signature token…");
            }
            ["submit", token] => {
                m::submit_labeler_plc_update(token);
                game.native_chat("atmospheric: submitting the PLC identity update…");
            }
            _ => game.native_chat(
                "atmospheric: /atmospheric labeler-identity [status|request|submit <token>]",
            ),
        }
    } else if rest == "profile-set" || rest.starts_with("profile-set ") {
        let payload = rest.strip_prefix("profile-set").unwrap_or("").trim_start();
        let (field, value) = payload.split_once(' ').unwrap_or((payload, ""));
        let (field, value) = (field.trim(), value.trim());
        *ran = true;
        if !matches!(field, "name" | "description" | "pronouns") {
            game.native_chat(
                "atmospheric: /atmospheric profile-set <name|description|pronouns> [text]",
            );
        } else {
            m::set_profile_field(field, value);
            let message = if value.is_empty() {
                format!("atmospheric: clearing profile {field}…")
            } else {
                format!("atmospheric: updating profile {field}…")
            };
            game.native_chat(&message);
        }
    } else if rest == "profile" || rest.starts_with("profile ") {
        let subject = rest.strip_prefix("profile").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat("atmospheric: /atmospheric profile <did | handle>");
        } else {
            m::fetch_profile(subject);
            game.native_chat("atmospheric: reading the profile…");
        }
    } else if rest == "player" || rest.starts_with("player ") {
        let subject = rest.strip_prefix("player").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat("atmospheric: /atmospheric player <did | handle>");
        } else {
            m::fetch_player(subject);
            game.native_chat("atmospheric: reading the player's snapshot record…");
        }
    } else if rest == "skin-of" || rest.starts_with("skin-of ") {
        let subject = rest.strip_prefix("skin-of").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat("atmospheric: /atmospheric skin-of <did | handle>");
        } else {
            m::fetch_player_skin(subject);
            game.native_chat("atmospheric: reading the player's skin record…");
        }
    } else if rest == "server-of" || rest.starts_with("server-of ") {
        let subject = rest.strip_prefix("server-of").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat("atmospheric: /atmospheric server-of <did | handle>");
        } else {
            m::fetch_server_profile(subject);
            game.native_chat("atmospheric: reading the server profile record…");
        }
    } else if rest == "chat-of" || rest.starts_with("chat-of ") {
        let subject = rest.strip_prefix("chat-of").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat("atmospheric: /atmospheric chat-of <did | handle>");
        } else {
            m::fetch_chat(subject);
            game.native_chat("atmospheric: reading the player's chat history…");
        }
    } else if rest == "records" || rest.starts_with("records ") {
        let mut args = rest
            .strip_prefix("records")
            .unwrap_or("")
            .trim()
            .splitn(2, ' ');
        let subject = args.next().unwrap_or("").trim();
        let collection = args.next().unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() || collection.is_empty() {
            game.native_chat("atmospheric: /atmospheric records <did | handle> <collection>");
        } else {
            m::fetch_records(subject, collection);
            game.native_chat("atmospheric: reading records…");
        }
    } else if rest == "reply" || rest.starts_with("reply ") {
        let mut args = rest.strip_prefix("reply").unwrap_or("").splitn(2, ' ');
        let uri = args.next().unwrap_or("").trim();
        let text = args.next().unwrap_or("").trim();
        *ran = true;
        if uri.is_empty() || text.is_empty() {
            game.native_chat("atmospheric: /atmospheric reply <at-uri> <text>");
        } else {
            m::reply(uri, text);
            game.native_chat("atmospheric: replying…");
        }
    } else if rest == "like" || rest.starts_with("like ") {
        let uri = rest.strip_prefix("like").unwrap_or("").trim();
        *ran = true;
        if uri.is_empty() {
            game.native_chat("atmospheric: /atmospheric like <at-uri>");
        } else {
            m::like(uri);
            game.native_chat("atmospheric: liking…");
        }
    } else if rest == "repost" || rest.starts_with("repost ") {
        let uri = rest.strip_prefix("repost").unwrap_or("").trim();
        *ran = true;
        if uri.is_empty() {
            game.native_chat("atmospheric: /atmospheric repost <at-uri>");
        } else {
            m::repost(uri);
            game.native_chat("atmospheric: reposting…");
        }
    } else if ["follow", "unfollow", "block", "unblock", "mute", "unmute"]
        .iter()
        .any(|action| rest == *action || rest.starts_with(&format!("{action} ")))
    {
        let mut args = rest.splitn(2, ' ');
        let action = args.next().unwrap_or_default();
        let subject = args.next().unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat(&format!("atmospheric: /atmospheric {action} <did | handle>"));
        } else {
            match action {
                "follow" => m::follow(subject),
                "unfollow" => m::unfollow(subject),
                "block" => m::block(subject),
                "unblock" => m::unblock(subject),
                "mute" => m::mute(subject),
                _ => m::unmute(subject),
            }
            let verb = match action {
                "follow" => "following",
                "unfollow" => "unfollowing",
                "block" => "blocking",
                "unblock" => "unblocking",
                "mute" => "muting",
                _ => "unmuting",
            };
            game.native_chat(&format!("atmospheric: {verb} {subject}…"));
        }
    } else if ["follows", "followers", "blocks", "mutes"]
        .iter()
        .any(|kind| rest == *kind || rest.starts_with(&format!("{kind} ")))
    {
        let mut args = rest.splitn(2, ' ');
        let kind = args.next().unwrap_or_default();
        let subject = args.next().unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat(&format!("atmospheric: /atmospheric {kind} <did | handle>"));
        } else {
            m::fetch_graph(kind, subject);
            game.native_chat(&format!("atmospheric: reading {kind}…"));
        }
    } else if rest == "chat" || rest.starts_with("chat ") {
        let text = rest.strip_prefix("chat").unwrap_or("").trim();
        *ran = true;
        if text.is_empty() {
            game.native_chat("atmospheric: /atmospheric chat <text>");
        } else {
            m::post_chat(text, false);
            game.native_chat("atmospheric: publishing the chat record…");
        }
    } else {
        *ran = true;
        game.native_chat(usage);
    }
}
