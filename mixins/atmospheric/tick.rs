// Once a tick on the game: collect the worker's events into the HUD status
// line, print incoming Colibri messages in a client window, and send M37's
// targeted profile prompts to joining players. Server ticks still drain events
// so a linked player is prompted even without a window.
fn atmospheric_tick(game: &mut crate::game::Game<'_>) {
    use crate::game::atmospheric as m;
    if !game.native_on(m::ID) {
        return;
    }
    // M17: spawn the sidecar worker binary once, if it is next to the game.
    let _ = m::ensure_attached();
    let _ = m::drain_events(16);
    if game.native_window() {
        for line in m::drain_colibri_chat() {
            game.native_chat(&line);
        }
    }
    for prompt in m::drain_colibri_prompts() {
        let Some(uuid) = m::uuid_value(&prompt.player_uuid) else {
            continue;
        };
        for line in prompt.lines {
            game.native_tell(uuid, horizon_client::text::Text::literal(&line));
        }
    }
    // M45: apply a linked player's Bluesky display name and pronouns as their
    // in-game name. The worker resolves the link and profile; a client tick
    // simply has no server to apply it to.
    for nickname in m::drain_player_nicknames() {
        let Some(uuid) = m::uuid_value(&nickname.player_uuid) else {
            continue;
        };
        game.native_set_nickname(uuid, Some(nickname.nickname.as_str()));
    }
    for link in m::drain_browser_links() {
        let Some(uuid) = m::uuid_value(&link.player_uuid) else {
            continue;
        };
        game.native_tell(
            uuid,
            horizon_client::text::Text::literal(&format!(
                "atmospheric: open {} to link your Bluesky account",
                link.url
            )),
        );
    }
    // M42: sweep the online players against the join gate. The worker checks
    // the list through its M52 cache, refreshing when stale, and reports the
    // linked players whose DID has left it; each is disconnected with
    // atpcraft's reason. The worker never schedules this itself, so the tick is
    // the only clock.
    let join_gate_due = m::join_gate_sweep_due();
    let labeler_due = m::labeler_sweep_due();
    if join_gate_due || labeler_due {
        let server_players = game.native_server_players();
        if join_gate_due {
            let players: Vec<m::SweepPlayer> = server_players
                .iter()
                .map(|player| m::SweepPlayer {
                    player_uuid: m::uuid_text(player.uuid),
                    player_name: player.name.clone(),
                })
                .collect();
            m::join_gate_sweep(players);
        }
        if labeler_due {
            // M62: capture the op level and vanilla statistics locally. Only
            // the resulting label values ever leave the game.
            let players: Vec<m::LabelerPlayer> = server_players
                .into_iter()
                .map(|player| m::LabelerPlayer {
                    player_uuid: m::uuid_text(player.uuid),
                    player_name: player.name,
                    admin: game
                        .native_player_permission(player.uuid)
                        .is_some_and(|level| level >= 1),
                    play_time_ticks: game
                        .native_player_stat(
                            player.uuid,
                            "minecraft:custom",
                            "minecraft:play_time",
                        )
                        .unwrap_or(0)
                        .max(0) as u64,
                    mob_kills: game
                        .native_player_stat(
                            player.uuid,
                            "minecraft:custom",
                            "minecraft:mob_kills",
                        )
                        .unwrap_or(0)
                        .max(0) as u64,
                    deaths: game
                        .native_player_stat(player.uuid, "minecraft:custom", "minecraft:deaths")
                        .unwrap_or(0)
                        .max(0) as u64,
                    dragon_kills: game
                        .native_player_stat(
                            player.uuid,
                            "minecraft:killed",
                            "minecraft:ender_dragon",
                        )
                        .unwrap_or(0)
                        .max(0) as u64,
                })
                .collect();
            m::labeler_sweep(players);
        }
    }
    for kicked in m::drain_join_gate_kicks() {
        game.native_run_command(&format!(
            "kick {} Removed from the server's Bluesky whitelist.",
            kicked.player_name
        ));
    }
}
