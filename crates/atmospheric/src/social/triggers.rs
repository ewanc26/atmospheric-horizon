//! Player-activity label triggers (M61, part of #112).
//!
//! atpcraft asserts two status labels from game events: `player`, a rolling
//! activity badge, and `online`, true only while the player is connected. It
//! negates `online` on logout, re-evaluates online players periodically, and
//! clears stale `online` labels at startup after an unclean stop.
//!
//! This module owns the pure evaluation and the two HTTP calls. The worker
//! resolves a Minecraft UUID to its linked DID through the identity service,
//! reads the labeler's active labels, then asks the same service to sign the
//! missing ones. Wire shapes come from the public lexicons only; no atpcraft
//! code is copied (its repo carries no licence).
//!
//! One deliberate difference: `online` carries no expiry here. atpcraft gives
//! it a 30-second expiry because its labeler lives inside the game process;
//! ours runs in `atmospheric-server`, survives a game restart, and clears
//! stale labels at worker startup, so an expiry would only add churn.

use crate::error::{AtmosphericError, Result};
use crate::labels::LabelDigest;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// The rolling activity badge: the player was seen on this server.
pub const PLAYER_LABEL: &str = "player";

/// The dynamic presence label: the player is connected now.
pub const ONLINE_LABEL: &str = "online";

/// How long a `player` label lasts without being refreshed (30 days).
pub const PLAYER_LABEL_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

/// How stale a `player` label may be before the trigger refreshes it (1 day).
pub const PLAYER_LABEL_REFRESH_SECONDS: i64 = 24 * 60 * 60;

/// The labeler's admin emission route on the identity service.
pub const EMIT_PATH: &str = "/v1/labeler/labels";

/// One label the trigger wants written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelAction {
    pub val: &'static str,
    pub neg: bool,
    pub exp: Option<String>,
}

/// The active labels for one subject, as value → expiry.
///
/// `queryLabels` serves only active labels, so a negation never appears here.
pub fn active_labels(
    labels: impl IntoIterator<Item = LabelDigest>,
) -> BTreeMap<String, Option<String>> {
    labels
        .into_iter()
        .filter(|label| !label.neg)
        .map(|label| (label.val, label.exp))
        .collect()
}

/// The actions that bring an online player's labels up to date at `now`.
///
/// `player` is asserted when absent, and refreshed only when its stored expiry
/// is more than [`PLAYER_LABEL_REFRESH_SECONDS`] behind the fresh one. A label
/// stored without an expiry is never refreshed, matching atpcraft. `online`
/// is asserted when absent; the sweep handles weakness, logout the negation.
pub fn online_actions(active: &BTreeMap<String, Option<String>>, now: i64) -> Vec<LabelAction> {
    let fresh = format_utc(now + PLAYER_LABEL_TTL_SECONDS);
    let refresh_before = format_utc(now + PLAYER_LABEL_TTL_SECONDS - PLAYER_LABEL_REFRESH_SECONDS);
    let mut actions = Vec::new();
    match active.get(PLAYER_LABEL) {
        None => actions.push(LabelAction {
            val: PLAYER_LABEL,
            neg: false,
            exp: Some(fresh),
        }),
        Some(Some(expiry)) if expiry.as_str() < refresh_before.as_str() => {
            actions.push(LabelAction {
                val: PLAYER_LABEL,
                neg: false,
                exp: Some(fresh),
            })
        }
        _ => {}
    }
    if !active.contains_key(ONLINE_LABEL) {
        actions.push(LabelAction {
            val: ONLINE_LABEL,
            neg: false,
            exp: None,
        });
    }
    actions
}

/// The actions for a disconnect: negate `online` only, and only when active.
///
/// `player` is a rolling badge and deliberately survives a logout.
pub fn logout_actions(active: &BTreeMap<String, Option<String>>) -> Vec<LabelAction> {
    if active.contains_key(ONLINE_LABEL) {
        vec![LabelAction {
            val: ONLINE_LABEL,
            neg: true,
            exp: None,
        }]
    } else {
        Vec::new()
    }
}

/// The subject DIDs with an active `online` label, deduplicated and sorted.
///
/// Used by the startup pass that negates labels left behind by an unclean
/// game stop. The caller has already filtered expired labels by querying.
pub fn online_subjects(labels: &[LabelDigest]) -> Vec<String> {
    let mut subjects: Vec<String> = labels
        .iter()
        .filter(|label| !label.neg && label.val == ONLINE_LABEL)
        .map(|label| label.uri.clone())
        .collect();
    subjects.sort();
    subjects.dedup();
    subjects
}

/// A negation action for one label value.
pub fn negate(val: &'static str) -> LabelAction {
    LabelAction {
        val,
        neg: true,
        exp: None,
    }
}

/// Now, in unix seconds.
pub fn now_unix() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(_) => 0,
    }
}

/// RFC 3339 UTC with milliseconds, the shape the label lexicon expects.
pub fn format_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = of_day / 3_600;
    let minute = (of_day % 3_600) / 60;
    let second = of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

/// Howard Hinnant's civil-from-days algorithm: days since 1970-01-01 to
/// (year, month, day). Kept here so the triggers need no date dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// Read the labeler's active labels for `patterns` from the identity service.
///
/// This is the public `queryLabels` read; `patterns` may be DIDs or trailing
/// `*` prefixes, and `*` alone asks for every active label.
pub async fn fetch_active(
    http: &reqwest::Client,
    server_api_url: &str,
    patterns: &[String],
) -> Result<Vec<LabelDigest>> {
    crate::labels::query_labels(http, server_api_url, patterns).await
}

/// Sign and append one label through the labeler's admin route.
pub async fn emit(
    http: &reqwest::Client,
    server_api_url: &str,
    token: &str,
    uri: &str,
    action: &LabelAction,
) -> Result<()> {
    let mut body = json!({
        "uri": uri,
        "val": action.val,
        "neg": action.neg,
    });
    if let Some(exp) = &action.exp {
        body["exp"] = json!(exp);
    }
    let base = server_api_url.trim_end_matches('/');
    let response = http
        .post(format!("{base}{EMIT_PATH}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(AtmosphericError::protocol)?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.map_err(AtmosphericError::protocol)?;
    Err(AtmosphericError::Message(format!(
        "labeler emission failed with HTTP {status}: {body}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(value, expiry)| ((*value).to_string(), expiry.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn utc_timestamps_have_the_lexicon_shape() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20.000Z");
        // Leap day.
        assert_eq!(format_utc(1_709_164_800), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn a_new_player_gets_the_badge_and_presence() {
        let now = 1_700_000_000;
        let actions = online_actions(&BTreeMap::new(), now);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].val, PLAYER_LABEL);
        assert!(!actions[0].neg);
        assert_eq!(
            actions[0].exp.as_deref(),
            Some(format_utc(now + PLAYER_LABEL_TTL_SECONDS).as_str())
        );
        assert_eq!(actions[1].val, ONLINE_LABEL);
        assert_eq!(actions[1].exp, None);
    }

    #[test]
    fn a_fresh_badge_and_presence_need_no_action() {
        let now = 1_700_000_000;
        let fresh = format_utc(now + PLAYER_LABEL_TTL_SECONDS);
        let labels = active(&[(PLAYER_LABEL, Some(fresh.as_str())), (ONLINE_LABEL, None)]);
        assert!(online_actions(&labels, now).is_empty());
    }

    #[test]
    fn a_stale_badge_is_refreshed_once_a_day() {
        let now = 1_700_000_000;
        // 29 days old: due for a refresh.
        let stale = format_utc(now + PLAYER_LABEL_TTL_SECONDS - PLAYER_LABEL_REFRESH_SECONDS - 1);
        let actions = online_actions(
            &active(&[(PLAYER_LABEL, Some(stale.as_str())), (ONLINE_LABEL, None)]),
            now,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].val, PLAYER_LABEL);
        assert_eq!(
            actions[0].exp.as_deref(),
            Some(format_utc(now + PLAYER_LABEL_TTL_SECONDS).as_str())
        );

        // 23 hours old: not due yet (a fresh label is not re-emitted).
        let recent =
            format_utc(now + PLAYER_LABEL_TTL_SECONDS - PLAYER_LABEL_REFRESH_SECONDS + 3_600);
        assert!(
            online_actions(
                &active(&[(PLAYER_LABEL, Some(recent.as_str())), (ONLINE_LABEL, None)]),
                now
            )
            .is_empty()
        );

        // No stored expiry: never refreshed, matching atpcraft.
        assert!(
            online_actions(&active(&[(PLAYER_LABEL, None), (ONLINE_LABEL, None)]), now).is_empty()
        );
    }

    #[test]
    fn logout_negates_only_an_active_presence_label() {
        let with_online = active(&[(PLAYER_LABEL, None), (ONLINE_LABEL, None)]);
        let actions = logout_actions(&with_online);
        assert_eq!(actions, vec![negate(ONLINE_LABEL)]);

        assert!(logout_actions(&active(&[(PLAYER_LABEL, None)])).is_empty());
    }

    #[test]
    fn startup_recovery_targets_each_online_subject_once() {
        let label = |uri: &str, val: &str, neg: bool| LabelDigest {
            val: val.into(),
            src: "did:plc:labeler".into(),
            uri: uri.into(),
            neg,
            cts: "2026-11-15T00:00:00.000Z".into(),
            exp: None,
        };
        let labels = vec![
            label("did:plc:alice", ONLINE_LABEL, false),
            label("did:plc:alice", ONLINE_LABEL, false),
            label("did:plc:bob", PLAYER_LABEL, false),
            label("did:plc:carol", ONLINE_LABEL, true),
        ];
        assert_eq!(online_subjects(&labels), vec!["did:plc:alice".to_string()]);
    }
}
