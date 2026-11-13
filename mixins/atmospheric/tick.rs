// Once a tick on the window's game: collect the worker's events into the HUD
// status line. (The server's own tick ignores this: `native_window` is false.)
fn atmospheric_tick(game: &mut crate::game::Game<'_>) {
    use crate::game::atmospheric as m;
    if !game.native_window() || !game.native_on(m::ID) {
        return;
    }
    // M17: spawn the sidecar worker binary once, if it is next to the game.
    let _ = m::ensure_attached();
    let _ = m::drain_events(16);
    for line in m::drain_colibri_chat() {
        game.native_chat(&line);
    }
}
