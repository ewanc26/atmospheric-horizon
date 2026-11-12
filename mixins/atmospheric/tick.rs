// Once a tick on the window's game: collect the worker's events into the HUD
// status line. (The server's own tick ignores this: `native_window` is false.)
fn atmospheric_tick(game: &mut crate::game::Game<'_>) {
    use crate::game::atmospheric as m;
    if !game.native_window() || !game.native_on(m::ID) {
        return;
    }
    let _ = m::drain_events(16);
}
