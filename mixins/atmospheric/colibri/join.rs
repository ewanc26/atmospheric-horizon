// A player logged in (HEAD of player_logged_in): schedule the presence
// fallback, ask the worker to check their linked Colibri profile (M37), and
// ask for their linked Bluesky display name and pronouns (M45).
// The worker waits three seconds before writing the fallback, and only when
// the vanilla broadcast or a rewritten join line did not mark it handled.
fn atmospheric_colibri_join(player: u128, name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() {
        return;
    }
    let line = horizon_client::text::Text::translate(
        "multiplayer.player.joined",
        &[horizon_client::text::Text::literal(name)],
    );
    m::post_colibri_presence("join", &line.plain());
    m::check_colibri_player(player, name);
    m::check_player_profile(player, name);
}
