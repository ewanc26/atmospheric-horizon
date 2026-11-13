// A player the server has accepted is about to be placed (HEAD of
// player_login): ask the worker for the M38 join-gate decision. The wait is
// bounded; an attached worker that cannot verify refuses the join.
fn atmospheric_gate_login(
    player: u128,
    name: &str,
    cancel: &mut bool,
    reason: &mut Option<horizon_client::text::Text>,
) {
    use crate::game::atmospheric as m;
    let _ = name;
    let uuid = m::uuid_text(player);
    if let Some(message) = m::join_gate_refusal(&uuid) {
        *cancel = true;
        *reason = Some(horizon_client::text::Text::literal(&message));
    }
}
