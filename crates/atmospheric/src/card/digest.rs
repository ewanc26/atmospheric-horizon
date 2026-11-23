//! Reading another server's published server card (#191, M69 read side):
//! resolve the account, page the card collection from its own PDS
//! unauthenticated, pick the valid record (rkey `self` preferred, newest
//! otherwise) and turn it into a digest with a liveness verdict, mirroring
//! the M19 server-profile read. The card carries no signature of its own
//! (repo authentication only), so the liveness verdict says when the
//! author last republished, never that anyone vouched for the fields.
//!
//! Timestamp handling is deliberately dependency-free: AT Protocol records
//! are stamped in UTC RFC 3339, and the parser pins exactly that.

use crate::card::record::{CARD_COLLECTION, CARD_RKEY, CardRecord};
use crate::error::{AtmosphericError, Result};
use reqwest::Client;

/// A card is fresh while its `createdAt` is within this many seconds of the
/// reader's clock (#191). The publish side stamps a fresh `createdAt` on
/// every write, so a stale card means the server stopped republishing, not
/// that the record is merely old.
pub const CARD_FRESH_SECONDS: i64 = 3_600;

/// The liveness verdict on a card's `createdAt` recency.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CardLiveness {
    /// The record's `createdAt` parses and sits within
    /// [`CARD_FRESH_SECONDS`] of the reader's clock.
    Fresh,
    /// The record's `createdAt` parses but is older than that.
    Stale,
    /// The record has no parseable `createdAt`; recency is unknowable.
    Unknown,
}

/// The HUD digest of a server card: where it was read, the typed record, and
/// the liveness verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CardDigest {
    pub did: String,
    pub rkey: String,
    pub at_uri: String,
    /// The typed, validated record as published.
    pub record: CardRecord,
    /// The record's `createdAt` as published (empty when absent).
    pub published_at: String,
    /// Whole seconds between `published_at` and the reader's clock, when
    /// the timestamp parses.
    pub age_seconds: Option<i64>,
    pub liveness: CardLiveness,
}

/// Whole Unix seconds for an RFC 3339 UTC timestamp
/// ("2026-11-23T09:30:00.123Z", also "+00:00"), truncating fractional
/// seconds. Any other offset is rejected rather than shifted: AT Protocol
/// timestamps are UTC, so a different offset marks data this reader should
/// not guess about. Field ranges are checked; calendar depth (a February
/// 31st) is not.
pub fn epoch_seconds(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let year = iso.get(0..4)?.parse::<i64>().ok()?;
    let month = iso.get(5..7)?.parse::<i64>().ok()?;
    let day = iso.get(8..10)?.parse::<i64>().ok()?;
    let hour = iso.get(11..13)?.parse::<i64>().ok()?;
    let minute = iso.get(14..16)?.parse::<i64>().ok()?;
    let second = iso.get(17..19)?.parse::<i64>().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let mut rest = &iso[19..];
    if rest.starts_with('.') {
        let digits = rest[1..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len() - 1);
        rest = &rest[1 + digits..];
    }
    if !(rest == "Z" || rest == "+00:00") {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Howard Hinnant's days-from-civil algorithm: exact for the proleptic
/// Gregorian calendar, no tables.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let month_shift = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_shift + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The liveness verdict for a card whose `createdAt` parsed to
/// `published_epoch` (or did not parse), read at `now_epoch`. A timestamp
/// slightly in the future (clock skew between hosts) stays fresh.
pub fn liveness(published_epoch: Option<i64>, now_epoch: i64) -> CardLiveness {
    match published_epoch.map(|published| now_epoch - published) {
        Some(age) if age <= CARD_FRESH_SECONDS => CardLiveness::Fresh,
        Some(_) => CardLiveness::Stale,
        None => CardLiveness::Unknown,
    }
}

/// A structurally valid card record: parseable as [`CardRecord`] and
/// passing its caps and scheme validation.
fn structural_valid(value: &serde_json::Value) -> bool {
    serde_json::from_value::<CardRecord>(value.clone())
        .map(|record| record.validate().is_ok())
        .unwrap_or(false)
}

/// The valid record at rkey `self` preferred, otherwise the newest valid
/// record (`createdAt` descending). Returns `None` when the repo has no
/// valid card. Mirrors the M19 server-profile pick.
pub fn latest(records: &[(String, serde_json::Value)]) -> Option<(String, serde_json::Value)> {
    let mut self_record: Option<(String, serde_json::Value)> = None;
    let mut best: Option<(String, serde_json::Value)> = None;
    for (rkey, value) in records {
        if !structural_valid(value) {
            continue;
        }
        if rkey == CARD_RKEY {
            self_record = Some((rkey.clone(), value.clone()));
        } else if best.is_none() {
            best = Some((rkey.clone(), value.clone()));
        } else if let Some((_, best_value)) = &best
            && value["createdAt"].as_str() > best_value["createdAt"].as_str()
        {
            best = Some((rkey.clone(), value.clone()));
        }
    }
    self_record.or(best)
}

/// Build the digest of a card record without any network: the typed record
/// plus the liveness verdict from its `createdAt` recency against
/// `now_epoch`, the caller's clock, so this stays deterministic and testable.
pub fn digest(
    did: &str,
    rkey: &str,
    record: &serde_json::Value,
    now_epoch: i64,
) -> Result<CardDigest> {
    let parsed = serde_json::from_value::<CardRecord>(record.clone()).map_err(|error| {
        AtmosphericError::Message(format!(
            "the server card does not match its schema: {error}"
        ))
    })?;
    let published_at = record
        .get("createdAt")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let published_epoch = epoch_seconds(&published_at);
    let age_seconds = published_epoch.map(|published| now_epoch - published);
    Ok(CardDigest {
        did: did.to_string(),
        rkey: rkey.to_string(),
        at_uri: format!("at://{did}/{CARD_COLLECTION}/{rkey}"),
        record: parsed,
        published_at,
        age_seconds,
        liveness: liveness(published_epoch, now_epoch),
    })
}

/// Read a server's card: resolve the account (a DID or handle, not a record
/// URI), page the card collection from its own PDS unauthenticated, pick
/// the valid record, and build the digest. Returns the subject's DID, the
/// PDS read from, and the digest (`None` when the repo has no valid card).
/// `now_epoch` is whole Unix seconds from the caller's clock.
pub async fn fetch_card(
    http: &Client,
    resolver: &crate::oauth::Resolver,
    raw: &str,
    now_epoch: i64,
) -> Result<(String, String, Option<CardDigest>)> {
    let (did, pds) = crate::social::servers::resolve_server(http, resolver, raw).await?;
    let records = crate::social::servers::list_records(http, &pds, &did, CARD_COLLECTION).await?;
    let Some((rkey, value)) = latest(&records) else {
        return Ok((did, pds, None));
    };
    let card = digest(&did, &rkey, &value, now_epoch)?;
    Ok((did, pds, Some(card)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::record::{CARD_SHA256_LEN, CardPack};

    fn card_value() -> serde_json::Value {
        let record = CardRecord {
            server_name: "A shoebox".into(),
            address: "example.test:25565".into(),
            players_online: 3,
            players_max: Some(20),
            game_version: "26.3".into(),
            engine: "horizon".into(),
            engine_revision: Some("9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0".into()),
            packs: vec![CardPack {
                name: "atmospheric-pack".into(),
                source: "https://example.test/pack.hpack".into(),
                sha256: "a".repeat(CARD_SHA256_LEN),
            }],
            conduct: Some("https://example.test/rules".into()),
            software: "atmospheric-horizon/1.2.3".into(),
        };
        serde_json::to_value(record).unwrap()
    }

    fn with_created_at(value: &serde_json::Value, created_at: &str) -> serde_json::Value {
        let mut value = value.clone();
        value["createdAt"] = serde_json::json!(created_at);
        value
    }

    #[test]
    fn epoch_seconds_pins_utc_rfc3339() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("1970-01-02T00:00:00Z"), Some(86_400));
        // A leap day, exercising the civil-calendar algorithm.
        assert_eq!(epoch_seconds("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(
            epoch_seconds("2026-11-23T09:30:00.123Z"),
            Some(1_795_426_200)
        );
        assert_eq!(
            epoch_seconds("2026-11-23T09:30:00+00:00"),
            Some(1_795_426_200)
        );
    }

    #[test]
    fn epoch_seconds_rejects_what_it_cannot_know() {
        assert_eq!(epoch_seconds("not a timestamp"), None);
        assert_eq!(epoch_seconds("2026-11-23 09:30:00Z"), None);
        assert_eq!(epoch_seconds("2026-13-01T00:00:00Z"), None);
        assert_eq!(epoch_seconds("2026-11-23T25:30:00Z"), None);
        // A non-UTC offset is refused rather than shifted.
        assert_eq!(epoch_seconds("2026-11-23T09:30:00+02:00"), None);
    }

    #[test]
    fn liveness_is_fresh_then_stale_then_unknown() {
        let published = 1_795_426_200;
        assert_eq!(
            liveness(Some(published), published + CARD_FRESH_SECONDS),
            CardLiveness::Fresh
        );
        assert_eq!(
            liveness(Some(published), published + CARD_FRESH_SECONDS + 1),
            CardLiveness::Stale
        );
        // Clock skew half a minute into the future stays fresh.
        assert_eq!(
            liveness(Some(published), published - 30),
            CardLiveness::Fresh
        );
        assert_eq!(liveness(None, published), CardLiveness::Unknown);
    }

    #[test]
    fn latest_prefers_self_and_skips_invalid() {
        let card = card_value();
        let mut broken = card_value();
        broken["packs"][0]["source"] = serde_json::json!("http://insecure.test/pack.hpack");
        let records = vec![
            (
                "old".to_string(),
                with_created_at(&card, "2026-11-01T00:00:00Z"),
            ),
            ("broken".to_string(), broken),
            (
                "self".to_string(),
                with_created_at(&card, "2026-11-22T00:00:00Z"),
            ),
        ];
        let (rkey, _) = latest(&records).unwrap();
        assert_eq!(rkey, "self");
    }

    #[test]
    fn latest_picks_newest_created_at_without_self() {
        let card = card_value();
        let records = vec![
            (
                "a".to_string(),
                with_created_at(&card, "2026-11-10T00:00:00.000Z"),
            ),
            (
                "b".to_string(),
                with_created_at(&card, "2026-11-11T00:00:00.000Z"),
            ),
        ];
        let (rkey, _) = latest(&records).unwrap();
        assert_eq!(rkey, "b");

        assert!(latest(&[]).is_none());
    }

    #[test]
    fn digest_builds_the_fields_and_verdicts() {
        let value = with_created_at(&card_value(), "2026-11-23T09:30:00Z");
        let card = digest("did:plc:alice", CARD_RKEY, &value, 1_795_426_200 + 600).unwrap();
        assert_eq!(card.did, "did:plc:alice");
        assert_eq!(card.rkey, CARD_RKEY);
        assert_eq!(
            card.at_uri,
            format!("at://did:plc:alice/{CARD_COLLECTION}/{CARD_RKEY}")
        );
        assert_eq!(card.record.server_name, "A shoebox");
        assert_eq!(card.record.players_online, 3);
        assert_eq!(card.record.packs.len(), 1);
        assert_eq!(card.published_at, "2026-11-23T09:30:00Z");
        assert_eq!(card.age_seconds, Some(600));
        assert_eq!(card.liveness, CardLiveness::Fresh);

        let stale = digest(
            "did:plc:alice",
            CARD_RKEY,
            &value,
            1_795_426_200 + CARD_FRESH_SECONDS + 1,
        )
        .unwrap();
        assert_eq!(stale.liveness, CardLiveness::Stale);

        let unknown = digest("did:plc:alice", CARD_RKEY, &card_value(), 1_795_426_200).unwrap();
        assert_eq!(unknown.published_at, "");
        assert_eq!(unknown.age_seconds, None);
        assert_eq!(unknown.liveness, CardLiveness::Unknown);
    }

    #[test]
    fn digest_says_why_a_wrong_record_is_wrong() {
        let mut wrong = card_value();
        wrong["address"] = serde_json::json!("ex ample");
        assert!(digest("did:plc:alice", CARD_RKEY, &wrong, 0).is_err());
    }
}
