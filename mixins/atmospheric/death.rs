// A system message (HEAD of system_message): death messages arrive here as
// translatable "death.*" components (death_message routes them through
// system_message for the dying player). Hand those to the worker, which
// posts them to Bluesky when `post_death_messages` is enabled in the settings.
fn atmospheric_death(game: &mut crate::game::Game<'_>, text: &horizon_client::text::Text) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) || !game.native_playing() {
        return;
    }
    let component = text.component();
    let is_death = component
        .get("translate")
        .and_then(|value| value.as_str())
        .is_some_and(|key| key.starts_with("death."));
    if is_death {
        m::post_death_message(&text.plain());
    }
}
