// A player has been placed on the server (HEAD of player_logged_in, M68): ask
// the worker to fetch the newest verified snapshot from this server or a
// trusted peer and download its aspects. The existing tick applies them to the
// online player. The worker ignores the request silently when player-data sync
// is off; explicit /atmospheric player-data restore commands always run.
fn atmospheric_playerdata_join(player: u128, name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() || m::uuid_text(player).is_empty() {
        return;
    }
    m::fetch_player_data(&m::uuid_text(player), name, true);
}
