// The status line in the bottom-left corner; the debug screen takes the top
// ones, and a bottom corner while one of its charts is drawn there.
fn atmospheric_hud(
    game: &mut crate::game::Game<'_>,
    batch: &mut horizon_client::hud::HudBatch,
    corners: &mut horizon_client::extras_hud::Corners,
) {
    use crate::game::atmospheric as m;
    use horizon_client::extras::Corner;
    if !game.native_on(m::ID) || !game.native_corner_shown(Corner::BottomLeft) {
        return;
    }
    let line = m::status_line();
    if line.is_empty() {
        return; // nothing to show until the worker reports something
    }
    let w = game.native_text_width(&line) + 4.0;
    let h = 12.0;
    let (x, y) = corners.place(Corner::BottomLeft, w, h);
    game.native_fill(batch, [x, y, w, h], m::BACKGROUND);
    game.native_text(batch, &line, [x + 2.0, y + 2.0], m::INK, true);
}
