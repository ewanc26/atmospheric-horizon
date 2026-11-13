// A player logged in (HEAD of player_logged_in): schedule the presence
// fallback. The worker waits three seconds and writes the line only when the
// vanilla broadcast or a rewritten join line did not mark the event handled.
fn atmospheric_colibri_join(name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() {
        return;
    }
    let line = horizon_client::text::Text::translate(
        "multiplayer.player.joined",
        &[horizon_client::text::Text::literal(name)],
    );
    m::post_colibri_presence("join", &line.plain());
}
