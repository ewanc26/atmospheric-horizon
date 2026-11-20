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
    for upload in m::drain_skin_uploads() {
        let Some(uuid) = m::uuid_value(&upload.player_uuid) else {
            continue;
        };
        game.native_tell(
            uuid,
            horizon_client::text::Text::literal(&format!(
                "atmospheric: open {} to upload and publish your skin",
                upload.url
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
    // M63: the command hook only queues; inventories and statistics are read
    // here, outside the player turn Horizon guards.
    for capture in m::drain_player_captures() {
        if capture_player(game, &capture) {
            continue;
        }
        if let Some(uuid) = m::uuid_value(&capture.player_uuid) {
            game.native_tell(
                uuid,
                horizon_client::text::Text::literal(
                    "atmospheric: could not capture player data (is the player online and fully placed?)",
                ),
            );
        }
    }
    for scan in m::drain_rpg_scans() {
        if !scan_rpg(game, &scan)
            && let Some(uuid) = m::uuid_value(&scan.player_uuid)
        {
            game.native_tell(
                uuid,
                horizon_client::text::Text::literal(
                    "atmospheric: could not scan for RPG items (is the player online?)",
                ),
            );
        }
    }
    for (player_uuid, player_name, version, aspects) in m::drain_player_restores() {
        match apply_restore(game, &player_uuid, &aspects) {
            Ok(count) => {
                if let Some(uuid) = m::uuid_value(&player_uuid) {
                    game.native_tell(
                        uuid,
                        horizon_client::text::Text::literal(format!(
                            "atmospheric: restored {count} aspect(s) from snapshot v{version}"
                        )),
                    );
                }
            }
            Err(message) => {
                if let Some(uuid) = m::uuid_value(&player_uuid) {
                    game.native_tell(
                        uuid,
                        horizon_client::text::Text::literal(format!(
                            "atmospheric: could not restore {player_name}: {message}"
                        )),
                    );
                }
            }
        }
    }
}

const PLAYER_SLOTS: usize = 41;
const MAIN_SLOTS: usize = 36;
const ENDER_SLOTS: usize = 27;
const ASPECT_LIMIT: usize = 8 * 1024 * 1024;

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
    encoder.finish().ok().filter(|bytes| bytes.len() <= ASPECT_LIMIT)
}

fn read_nbt(bytes: &[u8]) -> Option<horizon_world::nbt::Tag> {
    use flate2::read::GzDecoder;
    use horizon_world::nbt::{Limits, read_named};
    use std::io::Read as _;
    if bytes.len() > ASPECT_LIMIT {
        return None;
    }
    let mut raw = Vec::new();
    GzDecoder::new(bytes).read_to_end(&mut raw).ok()?;
    read_named(&raw, Limits::default()).ok().map(|(named, _)| named.tag)
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

fn aspect_items(
    bytes: &[u8],
    key: &str,
    size: usize,
) -> Option<Vec<Option<horizon_client::items::ItemStack>>> {
    let tag = read_nbt(bytes)?;
    let list = tag.get(key)?;
    Some(horizon_items::item_nbt::decode_items(Some(list), size))
}

fn aspect_equipment(
    bytes: &[u8],
) -> Option<Vec<Option<horizon_client::items::ItemStack>>> {
    let tag = read_nbt(bytes)?;
    let Some(horizon_world::nbt::Tag::List(items)) = tag.get("equipment") else {
        return None;
    };
    let mut slots = vec![None; 5];
    for item in items {
        let slot = match item.get("Slot") {
            Some(horizon_world::nbt::Tag::Byte(value)) => *value as i32,
            Some(horizon_world::nbt::Tag::Short(value)) => i32::from(*value),
            Some(horizon_world::nbt::Tag::Int(value)) => *value,
            _ => 0,
        };
        let index = match slot {
            100 => 0,
            101 => 1,
            102 => 2,
            103 => 3,
            -106 => 4,
            0..=4 => slot as usize,
            _ => continue,
        };
        slots[index] = horizon_items::item_nbt::item_from_nbt(item);
    }
    Some(slots)
}

fn capture_player(game: &mut crate::game::Game<'_>, capture: &crate::game::atmospheric::SweepPlayer) -> bool {
    use crate::game::atmospheric as m;
    use crate::game::native_inventories::NativeInventoryKind;
    let Some(uuid) = m::uuid_value(&capture.player_uuid) else {
        return false;
    };
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
    let food = game.native_player_food(uuid).map_or(20, |(food, _, _)| i64::from(food));
    m::post_player_data(
        &capture.player_uuid,
        &capture.player_name,
        summary_for(&player, food),
        aspects,
        false,
    );
    true
}

fn apply_restore(
    game: &mut crate::game::Game<'_>,
    player_uuid: &str,
    aspects: &[crate::game::atmospheric::PlayerAspect],
) -> Result<usize, String> {
    use crate::game::atmospheric as m;
    use crate::game::native_inventories::NativeInventoryKind;
    let uuid = m::uuid_value(player_uuid).ok_or("invalid player UUID")?;
    if game.native_server_players().iter().all(|p| p.uuid != uuid) {
        return Err("player is not online".into());
    }
    let mut count = 0;
    for aspect in aspects {
        let bytes = m::base64_decode(&aspect.data).ok_or("aspect is not base64")?;
        match aspect.name.as_str() {
            "inventory" => {
                let slots = aspect_items(&bytes, "items", MAIN_SLOTS).ok_or("bad inventory")?;
                let Ok(mut current) = game.native_inventory_typed(uuid, NativeInventoryKind::Player)
                else {
                    return Err("could not read the current inventory".into());
                };
                current.slots[..MAIN_SLOTS].clone_from_slice(&slots);
                game.native_set_inventory_typed(uuid, NativeInventoryKind::Player, current.slots, None)
                    .map_err(|error| error.to_string())?;
                count += 1;
            }
            "ender_chest" => {
                let slots = aspect_items(&bytes, "items", ENDER_SLOTS).ok_or("bad ender chest")?;
                game.native_set_inventory_typed(uuid, NativeInventoryKind::Ender, slots, None)
                    .map_err(|error| error.to_string())?;
                count += 1;
            }
            "equipment" => {
                let equipment = aspect_equipment(&bytes).ok_or("bad equipment")?;
                let Ok(mut current) = game.native_inventory_typed(uuid, NativeInventoryKind::Player)
                else {
                    return Err("could not read the current inventory".into());
                };
                current.slots[MAIN_SLOTS..PLAYER_SLOTS].clone_from_slice(&equipment);
                game.native_set_inventory_typed(uuid, NativeInventoryKind::Player, current.slots, None)
                    .map_err(|error| error.to_string())?;
                count += 1;
            }
            "advancements" => {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| "bad advancements JSON")?;
                if !game.native_set_player_advancements(uuid, &value) {
                    return Err("could not apply advancements".into());
                }
                count += 1;
            }
            "stats" => {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| "bad statistics JSON")?;
                if !game.native_set_player_stats(uuid, &value) {
                    return Err("could not apply statistics".into());
                }
                count += 1;
            }
            _ => {}
        }
    }
    Ok(count)
}

fn rpg_key(item_id: &str, renamed: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let sanitize = |raw: &str| -> String {
        raw.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ".-_:~".contains(ch) {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    };
    let hash = |value: &str| {
        let digest = Sha256::digest(value.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>()[..8].to_string()
    };
    let raw = match renamed {
        Some(name) => format!("{}~{}", sanitize(item_id), hash(name)),
        None => sanitize(item_id),
    };
    let mut chars: Vec<char> = raw.chars().collect();
    if chars.len() > 50 {
        let prefix: String = chars.drain(..41).collect();
        chars = format!("{prefix}~{}", hash(&raw)).chars().collect();
    }
    chars.into_iter().collect()
}

fn item_title(item_id: &str) -> String {
    item_id
        .split_once(':')
        .map_or(item_id, |(_, path)| path)
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn scan_rpg(
    game: &mut crate::game::Game<'_>,
    scan: &crate::game::atmospheric::SweepPlayer,
) -> bool {
    use crate::game::atmospheric as m;
    use crate::game::native_inventories::NativeInventoryKind;
    use std::collections::BTreeMap;
    let Some(uuid) = m::uuid_value(&scan.player_uuid) else {
        return false;
    };
    if player_named(game, uuid).is_none() {
        return false;
    }
    let Some(stats) = game.native_player_stats(uuid) else {
        return false;
    };
    let mut found: BTreeMap<String, m::RpgDiscovery> = BTreeMap::new();
    for kind in ["minecraft:picked_up", "minecraft:crafted", "minecraft:used", "minecraft:broken"] {
        let Some(entries) = stats["stats"][kind].as_object() else {
            continue;
        };
        for (item_id, count) in entries {
            if count.as_i64().unwrap_or(0) <= 0
                || horizon_client::item_sync::item_equippable(item_id, game.res.items()).is_some()
            {
                continue;
            }
            let key = rpg_key(item_id, None);
            found.entry(key.clone()).or_insert_with(|| m::RpgDiscovery {
                key,
                item_id: item_id.clone(),
                title: item_title(item_id),
                renamed: false,
            });
        }
    }
    let mut stacks = Vec::new();
    if let Ok(inventory) = game.native_inventory_typed(uuid, NativeInventoryKind::Player) {
        stacks.extend(inventory.slots);
    }
    if let Ok(ender) = game.native_inventory_typed(uuid, NativeInventoryKind::Ender) {
        stacks.extend(ender.slots);
    }
    for stack in stacks.into_iter().flatten() {
        let Some(name) = stack
            .data()
            .and_then(|data| data.custom_name_plain())
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        if horizon_client::item_sync::item_equippable(stack.item.as_ref(), game.res.items())
            .is_some()
        {
            continue;
        }
        let key = rpg_key(&stack.item, Some(&name));
        found.entry(key.clone()).or_insert_with(|| m::RpgDiscovery {
            key,
            item_id: stack.item.to_string(),
            title: name,
            renamed: true,
        });
    }
    if found.is_empty() {
        if let Some(uuid) = m::uuid_value(&scan.player_uuid) {
            game.native_tell(
                uuid,
                horizon_client::text::Text::literal(
                    "atmospheric: no new RPG item discoveries in your statistics or inventory",
                ),
            );
        }
        return true;
    }
    m::publish_rpg_items(
        &scan.player_uuid,
        &scan.player_name,
        found.into_values().collect(),
    );
    true
}
