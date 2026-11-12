// After a finished F2 screenshot: remember where the game saved it, so the
// post-screenshot keybind can send that file. At HEAD the original body has
// not run yet, so `ui.screenshot_message` still holds this shot's path.
fn atmospheric_shot(
    game: &mut crate::game::Game<'_>,
    result: &std::result::Result<[u32; 2], String>,
) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) || result.is_err() {
        return;
    }
    if let Some(path) = &game.ui.screenshot_message {
        m::remember_screenshot(&path.to_string_lossy());
    }
}
