//! Reading Bluesky profiles — the client-side half of atpcraft's profile
//! pronouns, reimplemented from the lexicon and the public API semantics
//! only (its repo carries no licence).
//!
//! atpcraft reads a profile from two sources: the AppView's
//! `app.bsky.actor.getProfile`, then the account's own
//! `app.bsky.actor.profile` record at rkey `self`. The direct record's
//! `pronouns` value wins even when it is absent — a successful record read
//! clears an older AppView value — while a failed record read falls back to
//! the AppView value. Display names only fall back to the record when the
//! AppView has none. Both reads are unauthenticated.

use crate::error::{AtmosphericError, Result};
use crate::labels;
use crate::oauth::{pds_endpoint, resolve_did_document};
use reqwest::Client;

/// The `app.bsky.actor.profile` collection.
pub const PROFILE_COLLECTION: &str = "app.bsky.actor.profile";

/// The fixed record key of the profile singleton.
pub const PROFILE_RKEY: &str = "self";

/// What the HUD shows for a profile (M24).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileDigest {
    pub did: String,
    pub handle: String,
    pub display_name: String,
    pub description: String,
    pub pronouns: Option<String>,
    pub avatar: String,
}

/// atpcraft's pronoun normal form: trim the ends, collapse internal
/// whitespace to one space, and treat an empty result as no value.
pub fn normalize_pronouns(raw: Option<&str>) -> Option<String> {
    let normal = raw?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normal.is_empty()).then_some(normal)
}

/// Resolve a profile reference (a DID or handle — not a record URI) to the
/// account's DID and the PDS endpoint its DID document advertises.
pub async fn resolve_profile(
    http: &Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
) -> Result<(String, String)> {
    let reference = labels::parse_subject_reference(raw)?;
    let resolved = labels::resolve_subject(http, resolver, &reference).await?;
    if resolved.collection.is_some() {
        return Err(AtmosphericError::Message(
            "want an account (a DID or a handle), not a record URI".into(),
        ));
    }
    let did = resolved.pattern;
    let doc = resolve_did_document(http, resolver, &did).await?;
    let pds = pds_endpoint(&doc)?;
    Ok((did, pds))
}

/// `GET` a JSON endpoint, returning `None` for any HTTP or parse failure.
/// The profile command treats either source as optional, matching atpcraft's
/// fallback policy.
async fn get_json(http: &Client, url: &str, query: &[(&str, &str)]) -> Option<serde_json::Value> {
    let response = http.get(url).query(query).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    serde_json::from_str(&body).ok()
}

/// Build the digest from the two optional profile sources. `None` means the
/// source failed; at least one must have succeeded.
pub fn digest(
    did: &str,
    appview: Option<&serde_json::Value>,
    record: Option<&serde_json::Value>,
) -> Result<ProfileDigest> {
    if appview.is_none() && record.is_none() {
        return Err(AtmosphericError::Message(format!(
            "could not read the profile for {did}: both the AppView and the PDS lookup failed"
        )));
    }
    let appview = appview.unwrap_or(&serde_json::Value::Null);
    let record = record.unwrap_or(&serde_json::Value::Null);
    fn non_empty<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }
    // A successful direct-record read owns the pronouns field, including the
    // absence of it; only a failed read falls back to the AppView.
    let pronouns = if record.is_object() {
        normalize_pronouns(record.get("pronouns").and_then(|v| v.as_str()))
    } else {
        normalize_pronouns(appview.get("pronouns").and_then(|v| v.as_str()))
    };
    Ok(ProfileDigest {
        did: non_empty(appview, "did").unwrap_or(did).to_string(),
        handle: non_empty(appview, "handle").unwrap_or("").to_string(),
        display_name: non_empty(appview, "displayName")
            .or_else(|| non_empty(record, "displayName"))
            .unwrap_or("")
            .to_string(),
        description: non_empty(appview, "description")
            .or_else(|| non_empty(record, "description"))
            .unwrap_or("")
            .to_string(),
        pronouns,
        avatar: non_empty(appview, "avatar").unwrap_or("").to_string(),
    })
}

/// Read a profile by DID or handle from both the AppView and the direct PDS
/// record, with atpcraft's precedence applied in [`digest`]. Returns the
/// resolved DID, the PDS it read from, and the digest.
pub async fn fetch_profile(
    http: &Client,
    resolver: &crate::oauth::Resolver,
    appview: &str,
    raw: &str,
) -> Result<(String, String, ProfileDigest)> {
    let (did, pds) = resolve_profile(http, resolver, raw).await?;
    let appview_base = appview.trim_end_matches('/');
    let pds_base = pds.trim_end_matches('/');
    let appview_profile = get_json(
        http,
        &format!("{appview_base}/xrpc/app.bsky.actor.getProfile"),
        &[("actor", &did)],
    )
    .await;
    let direct_record = get_json(
        http,
        &format!("{pds_base}/xrpc/com.atproto.repo.getRecord"),
        &[
            ("repo", &did),
            ("collection", PROFILE_COLLECTION),
            ("rkey", PROFILE_RKEY),
        ],
    )
    .await
    .and_then(|value| value.get("value").cloned());
    let profile = digest(&did, appview_profile.as_ref(), direct_record.as_ref())?;
    Ok((did, pds, profile))
}

/// atpcraft's longest Minecraft player name.
pub const MINECRAFT_NAME_MAX_LEN: usize = 16;

/// How long the complete in-game nickname may be (M47). The base name follows
/// atpcraft's 16-character Minecraft limit; the plain-text pronoun suffix and
/// the overall display are capped separately because Horizon's nickname helper
/// has no styled chat-only form.
pub const NICKNAME_MAX_LEN: usize = 32;

/// Compose the in-game nickname for a linked player (M47), following
/// atpcraft's `sanitizeMinecraftNameWithFallback`: the display name reduced to
/// a valid Minecraft name, else the handle's username part, else the vanilla
/// name. A usable pronoun value is appended as ` (pronouns)` in plain text.
/// Returns `None` when the result is the vanilla name, so an unchanged player
/// is left untouched.
pub fn nickname_for(vanilla: &str, profile: &ProfileDigest) -> Option<String> {
    let base = minecraft_name(&profile.display_name)
        .or_else(|| handle_name(&profile.handle))
        .unwrap_or_else(|| vanilla.to_string());
    let mut name = base;
    if let Some(pronouns) = normalize_pronouns(profile.pronouns.as_deref()) {
        name.push_str(" (");
        name.push_str(&pronouns);
        name.push(')');
    }
    let name = truncate_chars(&name, NICKNAME_MAX_LEN);
    (!name.is_empty() && name != vanilla).then_some(name)
}

/// atpcraft's `sanitizeMinecraftName`: keep `[A-Za-z0-9_]` and ASCII
/// whitespace, drop everything else, collapse whitespace runs into `_`, and
/// cap the result at 16 characters. `None` when nothing valid remains.
fn minecraft_name(raw: &str) -> Option<String> {
    let kept: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || ch.is_ascii_whitespace())
        .collect();
    let name: String = kept
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .take(MINECRAFT_NAME_MAX_LEN)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// atpcraft's handle fallback: the part before the first dot, sanitized.
fn handle_name(handle: &str) -> Option<String> {
    let (username, _) = handle.split_once('.')?;
    if username.is_empty() {
        return None;
    }
    minecraft_name(username)
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        truncated.trim_end().to_string()
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pronouns_are_trimmed_and_internal_whitespace_is_collapsed() {
        assert_eq!(
            normalize_pronouns(Some("  they \t  them \n")),
            Some("they them".into())
        );
        assert_eq!(normalize_pronouns(Some("  ")), None);
        assert_eq!(normalize_pronouns(None), None);
        // Unicode text passes through unchanged.
        assert_eq!(normalize_pronouns(Some("elle")), Some("elle".into()));
    }

    #[test]
    fn direct_record_pronouns_win_over_the_appview() {
        let appview = serde_json::json!({
            "did": "did:plc:alice",
            "handle": "alice.test",
            "displayName": "Alice",
            "description": "AppView bio",
            "pronouns": "she/her",
        });
        let record = serde_json::json!({
            "displayName": "Alice Direct",
            "description": "Direct bio",
            "pronouns": "they/them",
        });
        let profile = digest("did:plc:alice", Some(&appview), Some(&record)).unwrap();
        assert_eq!(profile.handle, "alice.test");
        // The AppView display name wins when it is non-empty.
        assert_eq!(profile.display_name, "Alice");
        assert_eq!(profile.description, "AppView bio");
        assert_eq!(profile.pronouns.as_deref(), Some("they/them"));
    }

    #[test]
    fn a_successful_direct_record_without_pronouns_clears_the_appview_value() {
        let appview = serde_json::json!({
            "did": "did:plc:alice",
            "handle": "alice.test",
            "pronouns": "she/her",
        });
        let record = serde_json::json!({ "displayName": "Alice" });
        let profile = digest("did:plc:alice", Some(&appview), Some(&record)).unwrap();
        assert_eq!(profile.pronouns, None);
    }

    #[test]
    fn a_failed_direct_record_falls_back_to_the_appview_pronouns() {
        let appview = serde_json::json!({
            "did": "did:plc:alice",
            "handle": "alice.test",
            "pronouns": "  she / her  ",
        });
        let profile = digest("did:plc:alice", Some(&appview), None).unwrap();
        assert_eq!(profile.pronouns.as_deref(), Some("she / her"));
    }

    #[test]
    fn a_direct_only_profile_keeps_its_identity_and_normalises_the_handle() {
        let record = serde_json::json!({
            "displayName": "Alice",
            "description": "Direct bio",
            "pronouns": "they/them",
        });
        let profile = digest("did:plc:alice", None, Some(&record)).unwrap();
        assert_eq!(profile.did, "did:plc:alice");
        assert_eq!(profile.handle, "");
        assert_eq!(profile.display_name, "Alice");
        assert_eq!(profile.description, "Direct bio");
        assert_eq!(profile.pronouns.as_deref(), Some("they/them"));
        assert_eq!(profile.avatar, "");
    }

    #[test]
    fn both_sources_failing_is_an_error() {
        let error = digest("did:plc:alice", None, None).unwrap_err();
        assert!(error.to_string().contains("both the AppView"), "{error}");
    }

    fn nickname_profile(display_name: &str, pronouns: Option<&str>) -> ProfileDigest {
        ProfileDigest {
            did: "did:plc:alice".into(),
            handle: "alice.test".into(),
            display_name: display_name.into(),
            description: String::new(),
            pronouns: pronouns.map(str::to_string),
            avatar: String::new(),
        }
    }

    #[test]
    fn nickname_uses_the_display_name_and_pronouns() {
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Alice", Some("they/them"))).as_deref(),
            Some("Alice (they/them)")
        );
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Alice", None)).as_deref(),
            Some("Alice")
        );
    }

    #[test]
    fn nickname_falls_back_to_the_handle_username() {
        let mut profile = nickname_profile("", None);
        profile.handle = "almostzoey.bsky.social".into();
        assert_eq!(
            nickname_for("Alex", &profile).as_deref(),
            Some("almostzoey")
        );

        profile.pronouns = Some("they/them".into());
        assert_eq!(
            nickname_for("Steve", &profile).as_deref(),
            Some("almostzoey (they/them)")
        );

        // An invalid handle leaves the vanilla name; pronouns still show.
        profile.handle = "🌈.test".into();
        assert_eq!(
            nickname_for("Alex", &profile).as_deref(),
            Some("Alex (they/them)")
        );
    }

    #[test]
    fn nickname_sanitizes_the_display_name_like_atpcraft() {
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Léon✨09", None)).as_deref(),
            Some("Lon09")
        );
        assert_eq!(
            nickname_for("Alex", &nickname_profile("🌈rain·bow🌈", None)).as_deref(),
            Some("rainbow")
        );
        assert_eq!(
            nickname_for("Alex", &nickname_profile("dash-dot.", None)).as_deref(),
            Some("dashdot")
        );
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Cool \t Guy", None)).as_deref(),
            Some("Cool_Guy")
        );
        // The stripped emoji leaves no dangling underscore.
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Guy 🌈", None)).as_deref(),
            Some("Guy")
        );
        assert_eq!(
            nickname_for(
                "Alex",
                &nickname_profile("ABCDEFGHIJKLMNOPQRSTUVWXYZ", None)
            )
            .as_deref(),
            Some("ABCDEFGHIJKLMNOP")
        );
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Alice Smith", Some("they/them"))).as_deref(),
            Some("Alice_Smith (they/them)")
        );
    }

    #[test]
    fn nickname_keeps_the_vanilla_name_when_nothing_changes() {
        // A display name identical to the Minecraft name changes nothing.
        assert_eq!(nickname_for("Alex", &nickname_profile("Alex", None)), None);
        // With no display name and no usable handle, there is nothing to apply.
        let mut profile = nickname_profile("", None);
        profile.handle = String::new();
        assert_eq!(nickname_for("Alex", &profile), None);
    }

    #[test]
    fn nickname_caps_the_base_and_the_final_text() {
        let long = "A".repeat(40);
        assert_eq!(
            nickname_for("Alex", &nickname_profile(&long, None)).as_deref(),
            Some("AAAAAAAAAAAAAAAA")
        );
        let pronouns = "p".repeat(40);
        assert_eq!(
            nickname_for("Alex", &nickname_profile("Alice", Some(&pronouns)))
                .unwrap()
                .chars()
                .count(),
            NICKNAME_MAX_LEN
        );
    }

    #[test]
    fn nickname_strips_control_characters_and_collapses_whitespace() {
        assert_eq!(
            nickname_for(
                "Alex",
                &nickname_profile("  Alice\u{0007}\n  Smith  ", None)
            )
            .as_deref(),
            Some("Alice_Smith")
        );
    }
}
