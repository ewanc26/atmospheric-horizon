//! The typed server card record (#191, M69): a liveness-checked server
//! manifest the server account publishes at a fixed record key, so the
//! commit recency doubles as its liveness and unsigned fields stay plainly
//! attributable to their author. Everything here is plain data, caps and
//! validation; the worker publish loop and the later read/verify side
//! (#196's pre-join display) build on it.

use serde::{Deserialize, Serialize};

/// The card's collection.
// Authority-domain note: club.atmospheric presumes control of an
// atmospheric domain the author does not hold yet. The M69 pull request
// tracks the choice (an existing owned domain, or a purchased one);
// changing this constant is the only edit.
pub const CARD_COLLECTION: &str = "club.atmospheric.server.card";

/// The fixed record key the card is rewritten at (#191): one card per
/// account, so a fresh commit time is the liveness a reader checks.
pub const CARD_RKEY: &str = "self";

/// Longest accepted display name or pack name, in bytes.
pub const CARD_MAX_NAME: usize = 64;
/// Longest accepted address string, in bytes.
pub const CARD_MAX_ADDRESS: usize = 128;
/// Sane ceiling for a published population count.
pub const CARD_MAX_PLAYERS: u32 = 100_000;
/// Most pack entries one card may declare.
pub const CARD_MAX_PACKS: usize = 32;
/// Longest accepted source, conduct or software string, in bytes.
pub const CARD_MAX_URL: usize = 256;
/// Exact length of the pinned engine revision, a commit hash.
pub const CARD_REVISION_LEN: usize = 40;
/// Exact length of a pack checksum.
pub const CARD_SHA256_LEN: usize = 64;

/// One mod pack the server declares: a name, the source a client fetches it
/// from and the sha-256 checksum the fetch is verified against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardPack {
    /// Pack display name, at most CARD_MAX_NAME bytes.
    pub name: String,
    /// https URL or registry reference the pack is fetched from.
    pub source: String,
    /// Lower-hex sha-256 of the pack file, CARD_SHA256_LEN characters.
    pub sha256: String,
}

/// The server card published at CARD_COLLECTION / CARD_RKEY (#191, M69).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardRecord {
    /// Server display name.
    pub server_name: String,
    /// Connect address: host or host:port.
    pub address: String,
    /// Players online at publish time.
    pub players_online: u32,
    /// Player-slot ceiling when the server reports one.
    pub players_max: Option<u32>,
    /// Minecraft game version the server runs, e.g. "26.3".
    pub game_version: String,
    /// Engine identifier, e.g. "horizon".
    pub engine: String,
    /// The pinned engine revision from compatibility.json when known.
    pub engine_revision: Option<String>,
    /// Mod packs the server runs, each with its checksum.
    pub packs: Vec<CardPack>,
    /// https URL or at:// URI of the server's rules or conduct page.
    pub conduct: Option<String>,
    /// Publishing software, e.g. "atmospheric-horizon/1.2.3".
    pub software: String,
}

/// True when every byte is 0-9 or a-f: lower-case hex.
fn is_lower_hex(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl CardRecord {
    /// Validate before publishing: non-empty fields within their caps,
    /// lower-hex checksums and scheme rules. The reason is human-readable;
    /// the worker reports it as-is.
    pub fn validate(&self) -> Result<(), String> {
        if self.server_name.trim().is_empty() {
            return Err("server_name is empty".into());
        }
        if self.server_name.len() > CARD_MAX_NAME {
            return Err("server_name is too long".into());
        }
        if self.address.trim().is_empty() {
            return Err("address is empty".into());
        }
        if self.address.len() > CARD_MAX_ADDRESS || self.address.contains(char::is_whitespace) {
            return Err("address is not a plain host or host:port".into());
        }
        if self.players_online > CARD_MAX_PLAYERS {
            return Err("players_online is beyond the sane ceiling".into());
        }
        if self.players_max.is_some_and(|max| max > CARD_MAX_PLAYERS) {
            return Err("players_max is beyond the sane ceiling".into());
        }
        if self.game_version.trim().is_empty() || self.game_version.len() > CARD_MAX_NAME {
            return Err("game_version is empty or too long".into());
        }
        if self.engine.trim().is_empty() || self.engine.len() > CARD_MAX_NAME {
            return Err("engine is empty or too long".into());
        }
        if let Some(revision) = &self.engine_revision {
            if revision.len() != CARD_REVISION_LEN || !is_lower_hex(revision) {
                return Err("engine_revision must be a 40-character lower-hex commit hash".into());
            }
        }
        if self.packs.len() > CARD_MAX_PACKS {
            return Err("packs may declare at most CARD_MAX_PACKS entries".into());
        }
        for (index, pack) in self.packs.iter().enumerate() {
            let position = index + 1;
            if pack.name.trim().is_empty() || pack.name.len() > CARD_MAX_NAME {
                return Err(format!("pack {position} name is empty or too long"));
            }
            if !pack.source.starts_with("https://") || pack.source.len() > CARD_MAX_URL {
                return Err(format!("pack {position} source must be an https URL"));
            }
            if pack.sha256.len() != CARD_SHA256_LEN || !is_lower_hex(&pack.sha256) {
                return Err(format!(
                    "pack {position} sha256 must be {CARD_SHA256_LEN} lower-hex characters"
                ));
            }
        }
        if let Some(conduct) = &self.conduct {
            if !(conduct.starts_with("https://") || conduct.starts_with("at://"))
                || conduct.len() > CARD_MAX_URL
            {
                return Err("conduct must be an https URL or an at:// URI".into());
            }
        }
        if self.software.trim().is_empty() || self.software.len() > CARD_MAX_URL {
            return Err("software is empty or too long".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack() -> CardPack {
        CardPack {
            name: "atmospheric-pack".into(),
            source: "https://example.test/pack.hpack".into(),
            sha256: "a".repeat(CARD_SHA256_LEN),
        }
    }

    fn card() -> CardRecord {
        CardRecord {
            server_name: "A shoebox".into(),
            address: "example.test:25565".into(),
            players_online: 3,
            players_max: Some(20),
            game_version: "26.3".into(),
            engine: "horizon".into(),
            engine_revision: Some("9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0".into()),
            packs: vec![pack()],
            conduct: Some("https://example.test/rules".into()),
            software: "atmospheric-horizon/1.2.3".into(),
        }
    }

    #[test]
    fn wire_shape_pins_camel_case_json() {
        assert_eq!(
            serde_json::to_string(&card()).unwrap(),
            concat!(
                r#"{"serverName":"A shoebox","address":"example.test:25565""#,
                r#","playersOnline":3,"playersMax":20,"gameVersion":"26.3""#,
                r#","engine":"horizon","engineRevision":"9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0""#,
                r#","packs":[{"name":"atmospheric-pack","source":"https://example.test/pack.hpack"#,
                r#","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]"#,
                r#","conduct":"https://example.test/rules","software":"atmospheric-horizon/1.2.3"}"#
            )
        );
    }

    #[test]
    fn card_round_trips() {
        let json = serde_json::to_string(&card()).unwrap();
        let back: CardRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, card());
    }

    #[test]
    fn none_fields_serialize_to_null() {
        let mut record = card();
        record.players_max = None;
        record.engine_revision = None;
        record.conduct = None;
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""playersMax":null"#));
        assert!(json.contains(r#""engineRevision":null"#));
        assert!(json.contains(r#""conduct":null"#));
    }

    #[test]
    fn a_valid_card_passes_with_and_without_packs() {
        assert_eq!(card().validate(), Ok(()));
        let mut record = card();
        record.packs.clear();
        assert_eq!(record.validate(), Ok(()));
    }

    #[test]
    fn validation_rejects_bad_fields() {
        let mut record = card();
        record.server_name = "  ".into();
        assert!(record.validate().unwrap_err().contains("server_name"));

        let mut record = card();
        record.server_name = "x".repeat(CARD_MAX_NAME + 1);
        assert!(record.validate().unwrap_err().contains("server_name"));

        let mut record = card();
        record.address = "ex ample".into();
        assert!(record.validate().unwrap_err().contains("address"));

        let mut record = card();
        record.players_online = CARD_MAX_PLAYERS + 1;
        assert!(record.validate().unwrap_err().contains("players_online"));

        let mut record = card();
        record.engine_revision = Some("XYZ".into());
        assert!(record.validate().unwrap_err().contains("engine_revision"));
    }

    #[test]
    fn validation_rejects_bad_packs() {
        let mut record = card();
        record.packs = vec![CardPack {
            name: "".into(),
            source: "https://example.test/pack.hpack".into(),
            sha256: "a".repeat(CARD_SHA256_LEN),
        }];
        assert!(record.validate().unwrap_err().contains("pack 1 name"));

        let mut record = card();
        record.packs = vec![CardPack {
            name: "p".into(),
            source: "http://insecure.test/pack.hpack".into(),
            sha256: "a".repeat(CARD_SHA256_LEN),
        }];
        assert!(record.validate().unwrap_err().contains("pack 1 source"));

        let mut record = card();
        record.packs = vec![CardPack {
            name: "p".into(),
            source: "https://example.test/pack.hpack".into(),
            sha256: "A".repeat(CARD_SHA256_LEN),
        }];
        assert!(record.validate().unwrap_err().contains("pack 1 sha256"));
    }

    #[test]
    fn validation_caps_pack_count() {
        let mut record = card();
        record.packs = (0..=CARD_MAX_PACKS)
            .map(|index| CardPack {
                name: format!("pack-{index}"),
                source: "https://example.test/pack.hpack".into(),
                sha256: "a".repeat(CARD_SHA256_LEN),
            })
            .collect();
        assert!(record.validate().unwrap_err().contains("packs may declare"));
    }

    #[test]
    fn validation_checks_conduct_schemes() {
        let mut record = card();
        record.conduct = Some("http://example.test/rules".into());
        assert!(record.validate().unwrap_err().contains("conduct"));

        let mut record = card();
        record.conduct = Some("at://did:plc:x/docs/rules".into());
        assert_eq!(record.validate(), Ok(()));
    }
}
