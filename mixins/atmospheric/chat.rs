// Every chat line the player submits: mirror it to the worker, which
// publishes it as an `at.atpcraft.player.chat` record when chat publishing is
// enabled in the settings (atpcraft port, M5). Commands are not chat
// messages, so they are skipped here (the command hook handles those).
fn atmospheric_chat(game: &mut crate::game::Game<'_>, message: &str) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) || !game.native_playing() || message.starts_with('/') {
        return;
    }
    m::post_chat(message, true);
}
