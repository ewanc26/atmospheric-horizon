//! Deterministic discovery keys and record bodies for the two rpg.actor
//! equipment records. Pure data: the worker does the writes.

use crate::RpgDiscovery;
use sha2::{Digest, Sha256};

/// Lexicon cap on the `item` field of both records.
pub const MAX_ITEM_ID_LENGTH: usize = 50;
/// Lexicon cap on `title`.
pub const MAX_TITLE_LENGTH: usize = 100;
/// Lexicon cap on `context`.
pub const MAX_CONTEXT_LENGTH: usize = 200;
/// Length of the hexadecimal suffix used for renamed or shortened keys.
pub const HASH_LENGTH: usize = 8;
/// Separator legal in an ATProto record key.
pub const SEPARATOR: char = '~';

/// The provider attestation's collection (server account's repo).
pub const GIVE_COLLECTION: &str = "equipment.rpg.give";
/// The player-held item's collection (player's repo).
pub const ITEM_COLLECTION: &str = "equipment.rpg.item";
/// The only `kind` this integration writes.
pub const KIND_INVENTORY: &str = "inventory";

fn allowed(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ".-_:~".contains(ch)
}

fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|ch| if allowed(ch) { ch } else { '_' })
        .collect()
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..HASH_LENGTH]
        .to_string()
}

fn fit(key: String) -> String {
    if key.chars().count() <= MAX_ITEM_ID_LENGTH {
        return key;
    }
    let prefix: String = key
        .chars()
        .take(MAX_ITEM_ID_LENGTH - HASH_LENGTH - 1)
        .collect();
    format!("{prefix}{SEPARATOR}{}", short_hash(&key))
}

/// The key of a plain item type, e.g. `minecraft:diamond_sword`.
pub fn plain(item_id: &str) -> String {
    fit(sanitize(item_id))
}

/// The key of a renamed stack: the item id plus a short hash of its name.
#[cfg(test)]
pub fn renamed(item_id: &str, custom_name: &str) -> String {
    fit(format!(
        "{}{SEPARATOR}{}",
        sanitize(item_id),
        short_hash(custom_name)
    ))
}

/// The deterministic provider record key for one player and discovery.
pub fn give_rkey(did: &str, key: &str) -> String {
    format!(
        "{}{SEPARATOR}{key}",
        sanitize(did.strip_prefix("did:").unwrap_or(did))
    )
}

/// Validate a game-supplied key: non-empty, at most the lexicon cap and
/// record-key safe. The custom-name hash cannot be recomputed from `title`
/// because the record caps it independently of the key; the game-side scanner
/// computes it from the full stack name.
pub fn canonical_key(discovery: &RpgDiscovery) -> Option<String> {
    let key = &discovery.key;
    if key.is_empty()
        || key.chars().count() > MAX_ITEM_ID_LENGTH
        || key == "."
        || key == ".."
        || discovery.item_id.trim().is_empty()
        || !key.chars().all(allowed)
    {
        return None;
    }
    if !discovery.renamed && key != &plain(&discovery.item_id) {
        return None;
    }
    Some(key.clone())
}

fn title(value: &str) -> String {
    value.chars().take(MAX_TITLE_LENGTH).collect()
}

fn context(value: &str) -> String {
    value.chars().take(MAX_CONTEXT_LENGTH).collect()
}

/// The provider's `equipment.rpg.give` attestation. No icon is written.
pub fn give_record(
    recipient: &str,
    discovery: &RpgDiscovery,
    given_at: &str,
    context_line: Option<&str>,
) -> serde_json::Value {
    let mut record = serde_json::json!({
        "$type": GIVE_COLLECTION,
        "recipient": recipient,
        "item": discovery.key,
        "title": title(&discovery.title),
        "kind": KIND_INVENTORY,
        "givenAt": given_at,
    });
    if let Some(line) = context_line {
        record["context"] = serde_json::Value::String(context(line));
    }
    record
}

/// The player's own `equipment.rpg.item`, pointing back at the attestation.
pub fn item_record(
    discovery: &RpgDiscovery,
    give_uri: &str,
    provider: &str,
    accepted_at: &str,
    context_line: Option<&str>,
) -> serde_json::Value {
    let mut record = serde_json::json!({
        "$type": ITEM_COLLECTION,
        "item": discovery.key,
        "title": title(&discovery.title),
        "kind": KIND_INVENTORY,
        "give": give_uri,
        "provider": provider,
        "acceptedAt": accepted_at,
    });
    if let Some(line) = context_line {
        record["context"] = serde_json::Value::String(context(line));
    }
    record
}

/// The `at://` URI of a provider attestation.
pub fn give_uri(provider: &str, rkey: &str) -> String {
    format!("at://{provider}/{GIVE_COLLECTION}/{rkey}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(key: &str, item_id: &str, title: &str, renamed: bool) -> RpgDiscovery {
        RpgDiscovery {
            key: key.into(),
            item_id: item_id.into(),
            title: title.into(),
            renamed,
        }
    }

    #[test]
    fn plain_and_renamed_keys_match_the_atpcraft_rules() {
        assert_eq!(plain("minecraft:diamond_sword"), "minecraft:diamond_sword");
        let renamed = renamed("minecraft:diamond_sword", "Oathkeeper");
        assert!(renamed.starts_with("minecraft:diamond_sword~"));
        assert_eq!(
            renamed.len(),
            "minecraft:diamond_sword~".len() + HASH_LENGTH
        );
        assert!(renamed.chars().all(allowed));
    }

    #[test]
    fn long_ids_keep_a_readable_prefix_and_a_hash() {
        let id = format!("mod:{}", "x".repeat(80));
        let key = plain(&id);
        assert_eq!(key.chars().count(), MAX_ITEM_ID_LENGTH);
        assert!(key.contains(SEPARATOR));
    }

    #[test]
    fn give_keys_are_deterministic_and_record_key_safe() {
        let first = give_rkey("did:plc:abc", "minecraft:dirt");
        assert_eq!(first, "plc:abc~minecraft:dirt");
        assert_eq!(first, give_rkey("did:plc:abc", "minecraft:dirt"));
        assert!(first.chars().all(allowed));
    }

    #[test]
    fn canonical_key_rejects_a_tampered_or_stale_key() {
        let plain_item = discovery("minecraft:dirt", "minecraft:dirt", "Dirt", false);
        assert_eq!(
            canonical_key(&plain_item).as_deref(),
            Some("minecraft:dirt")
        );
        let custom = discovery(
            &renamed("minecraft:dirt", "Home Soil"),
            "minecraft:dirt",
            "Home Soil",
            true,
        );
        assert!(canonical_key(&custom).is_some());
        let bad = discovery("minecraft:dirt!", "minecraft:stone", "Stone", false);
        assert!(canonical_key(&bad).is_none());
    }

    #[test]
    fn records_carry_the_lexicon_fields_and_caps() {
        let long_title = "t".repeat(MAX_TITLE_LENGTH + 20);
        let item = discovery(
            &plain("minecraft:dirt"),
            "minecraft:dirt",
            &long_title,
            false,
        );
        let give = give_record(
            &"g".repeat(MAX_CONTEXT_LENGTH + 20),
            &item,
            "now",
            Some(&"c".repeat(500)),
        );
        assert_eq!(give["$type"], GIVE_COLLECTION);
        assert_eq!(give["kind"], KIND_INVENTORY);
        assert_eq!(
            give["title"].as_str().unwrap().chars().count(),
            MAX_TITLE_LENGTH
        );
        assert_eq!(
            give["context"].as_str().unwrap().chars().count(),
            MAX_CONTEXT_LENGTH
        );

        let player = item_record(&item, "at://provider/give/1", "did:plc:p", "now", None);
        assert_eq!(player["$type"], ITEM_COLLECTION);
        assert_eq!(player["give"], "at://provider/give/1");
        assert!(player.get("context").is_none());
    }
}
