// /atmospheric login logs in; /atmospheric post <text> posts text;
// /atmospheric notifications polls now (the worker also polls on a timer);
// /atmospheric chat <text> publishes one chat record (atpcraft port).
fn atmospheric_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) {
        return;
    }
    let mut parts = command.splitn(2, ' ');
    if parts.next() != Some("atmospheric") {
        return;
    }
    let usage = "atmospheric: try /atmospheric login, /atmospheric post <text>, /atmospheric chat <text>, /atmospheric snapshot or /atmospheric notifications";
    let Some(rest) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
        *ran = true;
        game.native_chat(usage);
        return;
    };
    if rest == "login" {
        *ran = true;
        m::login();
        game.native_chat("atmospheric: logging in…");
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
    } else if rest == "snapshot" {
        *ran = true;
        match game.native_player() {
            Some(p) => {
                m::post_snapshot(m::snapshot_from_player("", p.dimension, p.x, p.y, p.z));
                game.native_chat("atmospheric: publishing the snapshot record…");
            }
            None => game.native_chat("atmospheric: no player in this world"),
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
