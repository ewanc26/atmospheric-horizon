// A player is leaving the server (HEAD of native_player_logging_out, M68):
// capture their five supported aspects while they are still in their level and
// listed in native_server_players, then ask the worker to publish an endorsed
// snapshot. The worker ignores the command silently when player-data sync is
// off; explicit /atmospheric player-data capture commands always run.
fn atmospheric_playerdata_leave(game: &mut crate::game::Game<'_>, player: u128, name: &str) {
    use crate::game::atmospheric as m;
    if name.trim().is_empty() || m::uuid_text(player).is_empty() {
        return;
    }
    crate::game::atmospheric_playerdata::capture_player(game, player, name, true);
}
