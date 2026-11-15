// A player logged in (HEAD of player_logged_in): ask the worker to assert
// their M61 player-activity labels. The worker resolves the UUID to a linked
// DID and is silent when the labeler triggers are not configured.
fn atmospheric_labeler_join(player: u128, name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() {
        return;
    }
    m::labeler_player_joined(&m::uuid_text(player));
}
