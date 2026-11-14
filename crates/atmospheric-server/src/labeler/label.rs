use serde::{Deserialize, Serialize};

/// One `com.atproto.label.defs#label`. The optional `sig` field is produced
/// separately by [`super::LabelSigner`] as 64 raw bytes, so it is not part of
/// this signing input type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    /// Label format version; the spec currently requires `1`.
    pub ver: u32,
    /// The labeler service identity (`did:plc:…`), not the signing `did:key`.
    pub src: String,
    /// The labelled subject: a bare DID for an account or a full AT-URI.
    pub uri: String,
    /// Optional CID binding a record label to a specific record version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    /// The label value identifier, e.g. `online`.
    pub val: String,
    /// A negation label, which withdraws a previous label of the same value.
    #[serde(default, skip_serializing_if = "is_false")]
    pub neg: bool,
    /// Creation timestamp, ISO-8601.
    pub cts: String,
    /// Optional expiry timestamp, ISO-8601.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_the_lexicon_field_names_and_omits_absent_optionals() {
        let label = Label {
            ver: 1,
            src: "did:plc:labeler".into(),
            uri: "did:plc:alice".into(),
            cid: None,
            val: "online".into(),
            neg: false,
            cts: "2026-11-14T00:00:00.000Z".into(),
            exp: None,
        };
        let value = serde_json::to_value(&label).unwrap();
        assert_eq!(value["ver"], 1);
        assert_eq!(value["src"], "did:plc:labeler");
        assert_eq!(value["uri"], "did:plc:alice");
        assert_eq!(value["val"], "online");
        assert_eq!(value["cts"], "2026-11-14T00:00:00.000Z");
        assert!(value.get("cid").is_none());
        assert!(value.get("neg").is_none());
        assert!(value.get("exp").is_none());
    }
}
