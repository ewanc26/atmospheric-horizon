// After a finished F2 screenshot: remember where the game saved it, so the
// post-screenshot keybind can send that file. At HEAD the original body has
// not run yet, so `ui.screenshot_messages` still holds this shot's path; a
// world-icon save is not in that list and must not become the next post.
fn atmospheric_shot(
    game: &mut crate::game::Game<'_>,
    saved: &horizon_client::gfx::SavedScreenshot,
) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) || saved.result.is_err() {
        return;
    }
    if game.ui.screenshot_messages.contains(&saved.path) {
        m::remember_screenshot(&saved.path.to_string_lossy());
    }
}
