// The post key went down while playing: post the player's location.
fn atmospheric_key(game: &mut crate::game::Game<'_>, key: &str, pressed: bool) {
    use crate::game::atmospheric as m;
    // Turned off in Options > Mods: nothing until the next build leaves it out.
    if !game.native_on(m::ID) || !pressed || !game.native_playing() {
        return;
    }
    let bound = game.native_mod_key(m::ID, m::KEY_POST, m::KEY_POST_DEFAULT);
    if key != bound {
        return;
    }
    if let Some(p) = game.native_player() {
        m::post_text(&m::location_line(p.x, p.y, p.z, p.dimension));
    }
}
