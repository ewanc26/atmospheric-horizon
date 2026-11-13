// Once a tick on the game: collect the worker's events into the HUD status
// line, print incoming Colibri messages in a client window, and send M37's
// targeted profile prompts to joining players. Server ticks still drain events
// so a linked player is prompted even without a window.
fn atmospheric_tick(game: &mut crate::game::Game<'_>) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) {
        return;
    }
    // M17: spawn the sidecar worker binary once, if it is next to the game.
    let _ = m::ensure_attached();
    let _ = m::drain_events(16);
    if game.native_window() {
        for line in m::drain_colibri_chat() {
            game.native_chat(&line);
        }
    }
    for prompt in m::drain_colibri_prompts() {
        let Some(uuid) = m::uuid_value(&prompt.player_uuid) else {
            continue;
        };
        for line in prompt.lines {
            game.native_tell(uuid, horizon_client::text::Text::literal(&line));
        }
    }
    for link in m::drain_browser_links() {
        let Some(uuid) = m::uuid_value(&link.player_uuid) else {
            continue;
        };
        game.native_tell(
            uuid,
            horizon_client::text::Text::literal(&format!(
                "atmospheric: open {} to link your Bluesky account",
                link.url
            )),
        );
    }
}
