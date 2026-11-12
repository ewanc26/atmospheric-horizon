// /atmospheric login logs in; /atmospheric post <text> posts text.
fn atmospheric_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) {
        return;
    }
    let mut parts = command.splitn(2, ' ');
    if parts.next() != Some("atmospheric") {
        return;
    }
    let Some(rest) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
        *ran = true;
        game.native_chat("atmospheric: try /atmospheric login or /atmospheric post <text>");
        return;
    };
    if rest == "login" {
        *ran = true;
        m::login();
        game.native_chat("atmospheric: logging in…");
    } else if rest == "post" || rest.starts_with("post ") {
        let text = rest.strip_prefix("post").unwrap_or("").trim();
        *ran = true;
        if text.is_empty() {
            game.native_chat("atmospheric: /atmospheric post <text>");
        } else {
            m::post_text(text);
            game.native_chat("atmospheric: posting…");
        }
    } else {
        *ran = true;
        game.native_chat("atmospheric: unknown command; try /atmospheric login or /atmospheric post <text>");
    }
}
