// After a finished F2 screenshot: remember where the game saved it for the
// post-screenshot keybind, and offer it to the worker as an automatic post
// (#182). At HEAD the original body has not run yet, so `ui.screenshot_messages`
// still holds this shot's path; a world-icon save is not in that list and must
// neither become the next post nor be posted automatically. The worker drops
// automatic shots unless `post_screenshots` is enabled, so this is safe to do
// for every screenshot; the file is already written by the saver thread.
fn atmospheric_shot(
    game: &mut crate::game::Game<'_>,
    saved: &horizon_client::gfx::SavedScreenshot,
) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) || saved.result.is_err() {
        return;
    }
    if game.ui.screenshot_messages.contains(&saved.path) {
        let path = saved.path.to_string_lossy();
        m::remember_screenshot(&path);
        m::post_screenshot_automatic(&path, "Screenshot from Horizon");
    }
}
