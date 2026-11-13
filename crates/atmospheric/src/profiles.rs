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
}
