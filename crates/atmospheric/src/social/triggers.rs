//! Player-activity label triggers (M61–M62, part of #112).
//!
//! atpcraft asserts status labels from game events and milestone labels from
//! vanilla statistics: `player`, a rolling activity badge; `online`, true only
//! while connected; `admin`, true while the player's op level is at least 1;
//! three playtime tiers; and three achievement labels. It negates labels the
//! current state no longer justifies, re-evaluates online players periodically,
//! and clears stale `online` labels at startup after an unclean stop.
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
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// The rolling activity badge: the player was seen on this server.
pub const PLAYER_LABEL: &str = "player";

/// The dynamic presence label: the player is connected now.
pub const ONLINE_LABEL: &str = "online";

/// The dynamic operator label: the player's op level is at least 1.
pub const ADMIN_LABEL: &str = "admin";

/// The lowest playtime milestone (10 hours).
pub const PLAYTIME_BRONZE_LABEL: &str = "playtime-bronze";

/// The middle playtime milestone (100 hours).
pub const PLAYTIME_SILVER_LABEL: &str = "playtime-silver";

/// The highest playtime milestone (1000 hours).
pub const PLAYTIME_GOLD_LABEL: &str = "playtime-gold";

/// The achievement label for killing the Ender Dragon.
pub const DRAGON_SLAYER_LABEL: &str = "dragon-slayer";

/// The achievement label for 1000 mob kills.
pub const MONSTER_HUNTER_LABEL: &str = "monster-hunter";

/// The achievement label for 100 deaths.
pub const DEATH_DEFIER_LABEL: &str = "death-defier";

/// Vanilla counts play time in ticks; 72,000 ticks is one hour.
const PLAY_TIME_TICKS_PER_HOUR: u64 = 72_000;

/// Hours needed for the bronze playtime label.
const PLAYTIME_BRONZE_HOURS: u64 = 10;

/// Hours needed for the silver playtime label.
const PLAYTIME_SILVER_HOURS: u64 = 100;

/// Hours needed for the gold playtime label.
const PLAYTIME_GOLD_HOURS: u64 = 1_000;

/// Mob kills needed for `monster-hunter`.
const MONSTER_HUNTER_MOB_KILLS: u64 = 1_000;

/// Deaths needed for `death-defier`.
const DEATH_DEFIER_DEATHS: u64 = 100;

/// Dragon kills needed for `dragon-slayer`.
const DRAGON_SLAYER_DRAGON_KILLS: u64 = 1;

/// Every label this evaluator owns. `vip` and `donor` are deliberately absent:
/// they are manual grants and must never be negated by a trigger sweep.
const OWNED_LABELS: [&str; 9] = [
    PLAYER_LABEL,
    ONLINE_LABEL,
    ADMIN_LABEL,
    PLAYTIME_BRONZE_LABEL,
    PLAYTIME_SILVER_LABEL,
    PLAYTIME_GOLD_LABEL,
    DRAGON_SLAYER_LABEL,
    MONSTER_HUNTER_LABEL,
    DEATH_DEFIER_LABEL,
];

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

/// The game-side state a label evaluation needs.
///
/// All values are captured locally from Horizon's native reads; only the
/// resulting label values are sent to the identity service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerActivity {
    /// The player is connected. A normal sweep always sets this.
    pub online: bool,
    /// The player's op level is at least 1.
    pub admin: bool,
    /// Vanilla `minecraft:custom`/`minecraft:play_time`, in ticks.
    pub play_time_ticks: u64,
    /// Vanilla `minecraft:custom`/`minecraft:mob_kills`.
    pub mob_kills: u64,
    /// Vanilla `minecraft:custom`/`minecraft:deaths`.
    pub deaths: u64,
    /// Vanilla `minecraft:killed`/`minecraft:ender_dragon`.
    pub dragon_kills: u64,
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

/// The labels the captured activity justifies right now.
///
/// Only the highest playtime tier is wanted, so the diff negates the lower
/// tiers as the player climbs. `vip` and `donor` are never included.
fn desired_labels(activity: &PlayerActivity) -> BTreeSet<&'static str> {
    let mut desired = BTreeSet::new();
    desired.insert(PLAYER_LABEL);
    if activity.online {
        desired.insert(ONLINE_LABEL);
    }
    if activity.admin {
        desired.insert(ADMIN_LABEL);
    }
    let hours = activity.play_time_ticks / PLAY_TIME_TICKS_PER_HOUR;
    let playtime = if hours >= PLAYTIME_GOLD_HOURS {
        Some(PLAYTIME_GOLD_LABEL)
    } else if hours >= PLAYTIME_SILVER_HOURS {
        Some(PLAYTIME_SILVER_LABEL)
    } else if hours >= PLAYTIME_BRONZE_HOURS {
        Some(PLAYTIME_BRONZE_LABEL)
    } else {
        None
    };
    if let Some(playtime) = playtime {
        desired.insert(playtime);
    }
    if activity.dragon_kills >= DRAGON_SLAYER_DRAGON_KILLS {
        desired.insert(DRAGON_SLAYER_LABEL);
    }
    if activity.mob_kills >= MONSTER_HUNTER_MOB_KILLS {
        desired.insert(MONSTER_HUNTER_LABEL);
    }
    if activity.deaths >= DEATH_DEFIER_DEATHS {
        desired.insert(DEATH_DEFIER_LABEL);
    }
    desired
}

/// The actions that bring an online player's labels up to date at `now`.
///
/// `player` is asserted when absent, and refreshed only when its stored expiry
/// is more than [`PLAYER_LABEL_REFRESH_SECONDS`] behind the fresh one. A label
/// stored without an expiry is never refreshed, matching atpcraft. Every other
/// owned label is asserted when the captured activity justifies it and negated
/// when it does not. Labels outside [`OWNED_LABELS`] are left untouched.
pub fn online_actions(
    active: &BTreeMap<String, Option<String>>,
    activity: PlayerActivity,
    now: i64,
) -> Vec<LabelAction> {
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
    let desired = desired_labels(&activity);
    for label in OWNED_LABELS.iter().skip(1) {
        if desired.contains(label) && !active.contains_key(*label) {
            actions.push(LabelAction {
                val: label,
                neg: false,
                exp: None,
            });
        }
    }
    for label in OWNED_LABELS.iter() {
        if !desired.contains(label) && active.contains_key(*label) {
            actions.push(negate(label));
        }
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

    fn online() -> PlayerActivity {
        PlayerActivity {
            online: true,
            ..PlayerActivity::default()
        }
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
        let actions = online_actions(&BTreeMap::new(), online(), now);
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
        assert!(online_actions(&labels, online(), now).is_empty());
    }

    #[test]
    fn a_stale_badge_is_refreshed_once_a_day() {
        let now = 1_700_000_000;
        // 29 days old: due for a refresh.
        let stale = format_utc(now + PLAYER_LABEL_TTL_SECONDS - PLAYER_LABEL_REFRESH_SECONDS - 1);
        let actions = online_actions(
            &active(&[(PLAYER_LABEL, Some(stale.as_str())), (ONLINE_LABEL, None)]),
            online(),
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
                online(),
                now
            )
            .is_empty()
        );

        // No stored expiry: never refreshed, matching atpcraft.
        assert!(
            online_actions(
                &active(&[(PLAYER_LABEL, None), (ONLINE_LABEL, None)]),
                online(),
                now
            )
            .is_empty()
        );
    }

    #[test]
    fn admin_and_milestones_follow_the_captured_state() {
        let now = 1_700_000_000;
        let activity = PlayerActivity {
            admin: true,
            play_time_ticks: PLAYTIME_GOLD_HOURS * PLAY_TIME_TICKS_PER_HOUR,
            mob_kills: MONSTER_HUNTER_MOB_KILLS,
            deaths: DEATH_DEFIER_DEATHS,
            dragon_kills: DRAGON_SLAYER_DRAGON_KILLS,
            ..online()
        };
        let actions = online_actions(&BTreeMap::new(), activity, now);
        let values: Vec<&str> = actions
            .iter()
            .filter(|action| !action.neg)
            .map(|action| action.val)
            .collect();
        assert_eq!(
            values,
            vec![
                PLAYER_LABEL,
                ONLINE_LABEL,
                ADMIN_LABEL,
                PLAYTIME_GOLD_LABEL,
                DRAGON_SLAYER_LABEL,
                MONSTER_HUNTER_LABEL,
                DEATH_DEFIER_LABEL,
            ]
        );

        // One tick short of each lower threshold adds nothing.
        let below = PlayerActivity {
            play_time_ticks: PLAYTIME_BRONZE_HOURS * PLAY_TIME_TICKS_PER_HOUR - 1,
            mob_kills: MONSTER_HUNTER_MOB_KILLS - 1,
            deaths: DEATH_DEFIER_DEATHS - 1,
            ..online()
        };
        let actions = online_actions(
            &active(&[(PLAYER_LABEL, None), (ONLINE_LABEL, None)]),
            below,
            now,
        );
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn only_the_highest_playtime_tier_survives() {
        let now = 1_700_000_000;
        let bronze = active(&[
            (PLAYER_LABEL, None),
            (ONLINE_LABEL, None),
            (PLAYTIME_BRONZE_LABEL, None),
        ]);
        let activity = PlayerActivity {
            play_time_ticks: 100 * PLAY_TIME_TICKS_PER_HOUR,
            ..online()
        };
        let actions = online_actions(&bronze, activity, now);
        assert_eq!(
            actions,
            vec![
                LabelAction {
                    val: PLAYTIME_SILVER_LABEL,
                    neg: false,
                    exp: None,
                },
                negate(PLAYTIME_BRONZE_LABEL),
            ]
        );

        let bronze_only = active(&[(PLAYER_LABEL, None), (ONLINE_LABEL, None)]);
        let activity = PlayerActivity {
            play_time_ticks: 10 * PLAY_TIME_TICKS_PER_HOUR,
            ..online()
        };
        assert_eq!(
            online_actions(&bronze_only, activity, now),
            vec![LabelAction {
                val: PLAYTIME_BRONZE_LABEL,
                neg: false,
                exp: None,
            }]
        );

        let silver = active(&[
            (PLAYER_LABEL, None),
            (ONLINE_LABEL, None),
            (PLAYTIME_BRONZE_LABEL, None),
            (PLAYTIME_SILVER_LABEL, None),
        ]);
        let activity = PlayerActivity {
            play_time_ticks: 1_000 * PLAY_TIME_TICKS_PER_HOUR,
            ..online()
        };
        let actions = online_actions(&silver, activity, now);
        assert_eq!(
            actions,
            vec![
                LabelAction {
                    val: PLAYTIME_GOLD_LABEL,
                    neg: false,
                    exp: None,
                },
                negate(PLAYTIME_BRONZE_LABEL),
                negate(PLAYTIME_SILVER_LABEL),
            ]
        );
    }

    #[test]
    fn deop_and_lost_thresholds_are_negated() {
        let now = 1_700_000_000;
        let labels = active(&[
            (PLAYER_LABEL, None),
            (ONLINE_LABEL, None),
            (ADMIN_LABEL, None),
            (DRAGON_SLAYER_LABEL, None),
            (MONSTER_HUNTER_LABEL, None),
            (DEATH_DEFIER_LABEL, None),
        ]);
        let actions = online_actions(&labels, online(), now);
        assert_eq!(
            actions,
            vec![
                negate(ADMIN_LABEL),
                negate(DRAGON_SLAYER_LABEL),
                negate(MONSTER_HUNTER_LABEL),
                negate(DEATH_DEFIER_LABEL),
            ]
        );
    }

    #[test]
    fn manual_labels_are_never_negated() {
        let now = 1_700_000_000;
        let labels = active(&[
            (PLAYER_LABEL, None),
            (ONLINE_LABEL, None),
            ("vip", None),
            ("donor", None),
        ]);
        assert!(online_actions(&labels, online(), now).is_empty());
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
