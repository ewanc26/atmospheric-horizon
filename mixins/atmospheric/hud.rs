// The status lines in the bottom-left corner (up to STATUS_LINES_MAX, oldest
// first); the debug screen takes the top ones, and a bottom corner while one
// of its charts is drawn there.
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
    let lines = m::status_lines();
    if lines.is_empty() {
        return; // nothing to show until the worker reports something
    }
    let w = lines
        .iter()
        .map(|line| game.native_text_width(line))
        .fold(0.0, f32::max)
        + 4.0;
    let h = 12.0 * lines.len() as f32;
    let (x, y) = corners.place(Corner::BottomLeft, w, h);
    game.native_fill(batch, [x, y, w, h], m::BACKGROUND);
    for (i, line) in lines.iter().enumerate() {
        game.native_text(
            batch,
            line,
            [x + 2.0, y + 2.0 + 12.0 * i as f32],
            m::INK,
            true,
        );
    }
}
