// A player logged out (HEAD of player_logged_out): ask the worker to negate
// their `online` label (M61). The rolling `player` badge survives a logout.
fn atmospheric_labeler_leave(player: u128, name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() {
        return;
    }
    m::labeler_player_left(&m::uuid_text(player));
}
