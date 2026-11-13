// A player logged out (HEAD of player_logged_out): schedule the presence
// fallback. The disconnect event can arrive before the leave line, so the
// worker waits three seconds before deciding.
fn atmospheric_colibri_leave(name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() {
        return;
    }
    let line = horizon_client::text::Text::translate(
        "multiplayer.player.left",
        &[horizon_client::text::Text::literal(name)],
    );
    m::post_colibri_presence("leave", &line.plain());
}
