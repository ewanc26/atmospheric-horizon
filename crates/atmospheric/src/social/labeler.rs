//! The server account's `app.bsky.labeler.service` declaration (M59, part of
//! #112).
//!
//! The record advertises every curated player label and its client-facing
//! metadata, so the AppView and Bluesky clients can recognise the labeler and
//! its label values. It lives at rkey `self` in the server account's repo.
//! Wire shapes come from the public lexicon only; no atpcraft code is copied.

use crate::error::Result;
use serde::{Deserialize, Serialize};

/// The labeler service declaration collection.
pub const LABELER_SERVICE_COLLECTION: &str = "app.bsky.labeler.service";

/// The declaration is a singleton at the literal rkey `self`.
pub const LABELER_SERVICE_RKEY: &str = "self";

/// One curated label value and the English strings clients display for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

/// The player-label vocabulary this labeler declares.
///
/// The identifiers match atpcraft's vocabulary so labels are interoperable.
/// Status labels are dynamic; milestone labels are sticky.
pub const LABELER_LABELS: [LabelDefinition; 11] = [
    LabelDefinition {
        id: "player",
        name: "Player",
        description: "Has played on this Minecraft server",
    },
    LabelDefinition {
        id: "online",
        name: "Online",
        description: "Is currently online on this Minecraft server",
    },
    LabelDefinition {
        id: "admin",
        name: "Admin",
        description: "Has operator privileges on this Minecraft server",
    },
    LabelDefinition {
        id: "vip",
        name: "VIP",
        description: "Has VIP status on this Minecraft server",
    },
    LabelDefinition {
        id: "donor",
        name: "Donor",
        description: "Has supported this Minecraft server",
    },
    LabelDefinition {
        id: "playtime-bronze",
        name: "Playtime: Bronze",
        description: "Has at least 10 hours of playtime",
    },
    LabelDefinition {
        id: "playtime-silver",
        name: "Playtime: Silver",
        description: "Has at least 100 hours of playtime",
    },
    LabelDefinition {
        id: "playtime-gold",
        name: "Playtime: Gold",
        description: "Has at least 1000 hours of playtime",
    },
    LabelDefinition {
        id: "dragon-slayer",
        name: "Dragon Slayer",
        description: "Has defeated the Ender Dragon",
    },
    LabelDefinition {
        id: "monster-hunter",
        name: "Monster Hunter",
        description: "Has at least 1000 mob kills",
    },
    LabelDefinition {
        id: "death-defier",
        name: "Death Defier",
        description: "Has died at least 100 times",
    },
];

/// The `policies` block of the declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelerPolicies {
    pub label_values: Vec<String>,
    pub label_value_definitions: Vec<LabelValueDefinition>,
}

/// One `com.atproto.label.defs#labelValueDefinition` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelValueDefinition {
    pub identifier: String,
    pub severity: String,
    pub blurs: String,
    pub default_setting: String,
    pub adult_only: bool,
    pub locales: Vec<LabelValueLocale>,
}

/// The English display strings for a label value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelValueLocale {
    pub lang: String,
    pub name: String,
    pub description: String,
}

/// The `app.bsky.labeler.service` record body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelerServiceRecord {
    pub policies: LabelerPolicies,
    pub created_at: String,
}

impl LabelerServiceRecord {
    /// Build the full declaration, preserving the given creation timestamp.
    pub fn new(created_at: String) -> Self {
        let label_values = LABELER_LABELS
            .iter()
            .map(|label| label.id.to_string())
            .collect();
        let label_value_definitions = LABELER_LABELS
            .iter()
            .map(|label| LabelValueDefinition {
                identifier: label.id.into(),
                severity: "inform".into(),
                blurs: "none".into(),
                default_setting: "warn".into(),
                adult_only: false,
                locales: vec![LabelValueLocale {
                    lang: "en".into(),
                    name: label.name.into(),
                    description: label.description.into(),
                }],
            })
            .collect();
        Self {
            policies: LabelerPolicies {
                label_values,
                label_value_definitions,
            },
            created_at,
        }
    }

    /// The record body with the `$type` putRecord requires.
    pub fn to_unknown(&self) -> Result<atrium_api::types::Unknown> {
        crate::actions::record_to_unknown(LABELER_SERVICE_COLLECTION, self)
    }
}

/// The `createdAt` of an existing declaration, if it has a string value.
pub fn existing_created_at(record: &serde_json::Value) -> Option<String> {
    record
        .get("createdAt")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_lists_every_curated_label_and_definition() {
        let record = LabelerServiceRecord::new("2026-11-14T00:00:00.000Z".into());
        let value = serde_json::to_value(&record).unwrap();
        let expected: Vec<&str> = LABELER_LABELS.iter().map(|label| label.id).collect();

        assert_eq!(
            value["policies"]["labelValues"],
            serde_json::json!(expected)
        );
        let definitions = value["policies"]["labelValueDefinitions"]
            .as_array()
            .unwrap();
        assert_eq!(definitions.len(), expected.len());
        for (definition, id) in definitions.iter().zip(expected) {
            assert_eq!(definition["identifier"], id);
            assert_eq!(definition["severity"], "inform");
            assert_eq!(definition["blurs"], "none");
            assert_eq!(definition["defaultSetting"], "warn");
            assert_eq!(definition["adultOnly"], false);
            assert_eq!(definition["locales"][0]["lang"], "en");
            assert!(
                definition["locales"][0]["name"]
                    .as_str()
                    .is_some_and(|name| !name.is_empty())
            );
            assert!(
                definition["locales"][0]["description"]
                    .as_str()
                    .is_some_and(|description| !description.is_empty())
            );
        }
        assert_eq!(value["createdAt"], "2026-11-14T00:00:00.000Z");
    }

    #[test]
    fn record_gets_the_service_type_and_preserves_an_existing_timestamp() {
        let record = LabelerServiceRecord::new("2026-01-02T03:04:05.000Z".into());
        let value = serde_json::to_value(record.to_unknown().unwrap()).unwrap();
        assert_eq!(value["$type"], LABELER_SERVICE_COLLECTION);
        assert_eq!(value["createdAt"], "2026-01-02T03:04:05.000Z");

        assert_eq!(
            existing_created_at(&serde_json::json!({
                "createdAt": "2026-01-02T03:04:05.000Z"
            })),
            Some("2026-01-02T03:04:05.000Z".into())
        );
        assert_eq!(existing_created_at(&serde_json::json!({})), None);
        assert_eq!(
            existing_created_at(&serde_json::json!({"createdAt": 1})),
            None
        );
    }
}
