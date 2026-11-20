// Shared M63 aspect capture (M68). Injected as `crate::game::atmospheric_playerdata`
// so both the tick's manual `/atmospheric player-data capture` and the
// `player_logging_out` hook use exactly the same encoding. Helpers injected at
// one hook cannot call helpers injected at another by name, hence the module.
//
// The caller must be on the server tick, not inside a player's turn: Horizon
// refuses inventory access during a turn. At `native_player_logging_out` the
// player is still in their level and listed in `native_server_players`, so the
// UUID-based helpers still reach them.

const ASPECT_LIMIT: usize = 8 * 1024 * 1024;
const MAIN_SLOTS: usize = 36;

fn player_named(
    game: &mut crate::game::Game<'_>,
    uuid: u128,
) -> Option<horizon_client::mod_api::NativeServerPlayer> {
    game.native_server_players()
        .into_iter()
        .find(|player| player.uuid == uuid)
}

fn summary_for(
    player: &horizon_client::mod_api::NativeServerPlayer,
    food: i64,
) -> crate::game::atmospheric::SnapshotSummary {
    let mode = match player.game_mode {
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "survival",
    };
    crate::game::atmospheric::SnapshotSummary {
        username: player.name.clone(),
        dimension: player.dimension.clone(),
        x: player.position[0].floor() as i64,
        y: player.position[1].floor() as i64,
        z: player.position[2].floor() as i64,
        health: format!("{}", player.health),
        food,
        xp_level: i64::from(player.xp_level),
        game_mode: mode.into(),
    }
}

fn gzip_nbt(key: &str, tag: horizon_world::nbt::Tag) -> Option<Vec<u8>> {
    use flate2::{Compression, write::GzEncoder};
    use horizon_world::nbt::{Limits, NamedTag, write_named};
    use std::io::Write as _;
    let named = NamedTag {
        name: "".into(),
        tag: horizon_world::nbt::Tag::compound([(key, tag)]),
    };
    let raw = write_named(&named, Limits::default()).ok()?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).ok()?;
    encoder
        .finish()
        .ok()
        .filter(|bytes| bytes.len() <= ASPECT_LIMIT)
}

fn inventory_aspect(slots: &[Option<horizon_client::items::ItemStack>]) -> Option<Vec<u8>> {
    gzip_nbt(
        "items",
        horizon_items::item_nbt::encode_items(&slots[..MAIN_SLOTS]),
    )
}

fn ender_aspect(slots: &[Option<horizon_client::items::ItemStack>]) -> Option<Vec<u8>> {
    gzip_nbt("items", horizon_items::item_nbt::encode_items(slots))
}

fn equipment_aspect(slots: &[Option<horizon_client::items::ItemStack>]) -> Option<Vec<u8>> {
    let mut items = Vec::new();
    for (index, slot) in [(36, 100i8), (37, 101), (38, 102), (39, 103), (40, -106)] {
        let Some(stack) = slots.get(index).and_then(Option::as_ref) else {
            continue;
        };
        let mut tag = horizon_items::item_nbt::item_to_nbt(stack);
        if let horizon_world::nbt::Tag::Compound(map) = &mut tag {
            map.insert("Slot".into(), horizon_world::nbt::Tag::Byte(slot));
        }
        items.push(tag);
    }
    gzip_nbt("equipment", horizon_world::nbt::Tag::List(items))
}

/// Capture the five supported aspects of an online player and ask the worker
/// to publish them. `automatic` marks the M68 logout hook: the worker ignores
/// it silently when `player_data_sync_enabled` is off. An explicit capture
/// passes `false` and always runs.
pub fn capture_player(
    game: &mut crate::game::Game<'_>,
    uuid: u128,
    player_name: &str,
    automatic: bool,
) -> bool {
    use crate::game::atmospheric as m;
    use crate::game::native_inventories::NativeInventoryKind;
    let Some(player) = player_named(game, uuid) else {
        return false;
    };
    let Ok(inventory) = game.native_inventory_typed(uuid, NativeInventoryKind::Player) else {
        return false;
    };
    let Ok(ender) = game.native_inventory_typed(uuid, NativeInventoryKind::Ender) else {
        return false;
    };
    let mut aspects = Vec::new();
    if let Some(bytes) = inventory_aspect(&inventory.slots) {
        aspects.push(m::PlayerAspect {
            name: "inventory".into(),
            data: m::base64_encode(&bytes),
        });
    }
    if let Some(bytes) = ender_aspect(&ender.slots) {
        aspects.push(m::PlayerAspect {
            name: "ender_chest".into(),
            data: m::base64_encode(&bytes),
        });
    }
    if let Some(bytes) = equipment_aspect(&inventory.slots) {
        aspects.push(m::PlayerAspect {
            name: "equipment".into(),
            data: m::base64_encode(&bytes),
        });
    }
    if let Some(stats) = game.native_player_stats(uuid)
        && let Ok(bytes) = serde_json::to_vec(&stats)
    {
        aspects.push(m::PlayerAspect {
            name: "stats".into(),
            data: m::base64_encode(&bytes),
        });
    }
    if let Some(advancements) = game.native_player_advancements(uuid)
        && let Ok(bytes) = serde_json::to_vec(&advancements)
    {
        aspects.push(m::PlayerAspect {
            name: "advancements".into(),
            data: m::base64_encode(&bytes),
        });
    }
    if aspects.is_empty() {
        return false;
    }
    let food = game
        .native_player_food(uuid)
        .map_or(20, |(food, _, _)| i64::from(food));
    let player_uuid = m::uuid_text(uuid);
    m::post_player_data(
        &player_uuid,
        player_name,
        summary_for(&player, food),
        aspects,
        automatic,
    );
    true
}
