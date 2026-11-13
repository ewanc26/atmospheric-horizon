// /atmospheric login [handle] logs in (app password from the settings, or
// localhost OAuth when a handle is given); /atmospheric post <text> posts
// text; /atmospheric notifications polls now (the worker also polls on a
// timer); /atmospheric chat <text> publishes one chat record (atpcraft port);
// /atmospheric skin [path] [default|slim] publishes a skin record (M9);
// /atmospheric list <reference> reads a public list (M11); /atmospheric labels
// <subject> reads labels off a subject's AppView view and /atmospheric labeler
// <did> [subject] queries a labeler directly (M12); /atmospheric player
// <did|handle> reads a player's latest verified snapshot record (M13).
fn atmospheric_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) {
        return;
    }
    let mut parts = command.splitn(2, ' ');
    if parts.next() != Some("atmospheric") {
        return;
    }
    let usage = "atmospheric: try /atmospheric login [handle], /atmospheric post <text>, /atmospheric chat <text>, /atmospheric snapshot, /atmospheric skin [path] [default|slim], /atmospheric server, /atmospheric list <list>, /atmospheric labels <subject>, /atmospheric labeler <did> [subject], /atmospheric player <did|handle>, /atmospheric logout or /atmospheric notifications";
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
    } else if rest == "player" || rest.starts_with("player ") {
        let subject = rest.strip_prefix("player").unwrap_or("").trim();
        *ran = true;
        if subject.is_empty() {
            game.native_chat("atmospheric: /atmospheric player <did | handle>");
        } else {
            m::fetch_player(subject);
            game.native_chat("atmospheric: reading the player's snapshot record…");
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
