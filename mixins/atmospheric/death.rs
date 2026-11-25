// System messages (HEAD of system_message): death messages arrive here as
// translatable "death.*" components, and advancement announcements as
// "chat.type.advancement.*" components. Hand those to the worker, which posts
// them to Bluesky when post_death_messages / post_achievement_messages are
// enabled in the settings. Both are broadcast lines, so every linked client
// that opts in posts from its own account, mirroring the existing death path.
fn atmospheric_death(game: &mut crate::game::Game<'_>, text: &horizon_client::text::Text) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) || !game.native_playing() {
        return;
    }
    let key = text
        .component()
        .get("translate")
        .and_then(|value| value.as_str());
    let Some(key) = key else {
        return;
    };
    if key.starts_with("death.") {
        m::post_death_message(&text.plain());
    } else if key.starts_with("chat.type.advancement.") {
        m::post_achievement(&text.plain());
    }
}
