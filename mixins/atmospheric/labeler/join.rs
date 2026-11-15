// A player logged in (HEAD of player_logged_in): force the next tick's label
// sweep so their M61/M62 labels are captured without waiting for the minute.
// The worker resolves the UUID to a linked DID and is silent when the labeler
// triggers are not configured.
fn atmospheric_labeler_join(player: u128, name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() || m::uuid_text(player).is_empty() {
        return;
    }
    m::labeler_player_joined();
}
