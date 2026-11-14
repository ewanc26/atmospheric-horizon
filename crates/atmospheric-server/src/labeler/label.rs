use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};

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

impl Label {
    /// Whether this label's `exp` has passed. A missing or unparseable expiry
    /// means the label does not expire, matching atpcraft's fallback.
    pub fn is_expired(&self, now: u64) -> bool {
        self.exp
            .as_deref()
            .and_then(super::time::parse_rfc3339)
            .is_some_and(|expiry| expiry < now as i64)
    }
}

/// A signed label as it appears on the wire and in the label log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedLabel {
    pub label: Label,
    pub sig: [u8; 64],
}

impl SignedLabel {
    pub fn new(label: Label, sig: [u8; 64]) -> Self {
        Self { label, sig }
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.label.is_expired(now)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct WireRef<'a> {
    ver: u32,
    src: &'a str,
    uri: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cid: Option<&'a str>,
    val: &'a str,
    #[serde(skip_serializing_if = "is_false")]
    neg: bool,
    cts: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exp: Option<&'a str>,
    sig: SignatureRef<'a>,
}

#[derive(Serialize)]
struct SignatureRef<'a> {
    #[serde(rename = "$bytes")]
    bytes: &'a str,
}

impl Serialize for SignedLabel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let signature = STANDARD_NO_PAD.encode(self.sig);
        WireRef {
            ver: self.label.ver,
            src: &self.label.src,
            uri: &self.label.uri,
            cid: self.label.cid.as_deref(),
            val: &self.label.val,
            neg: self.label.neg,
            cts: &self.label.cts,
            exp: self.label.exp.as_deref(),
            sig: SignatureRef { bytes: &signature },
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
struct WireOwned {
    ver: u32,
    src: String,
    uri: String,
    #[serde(default)]
    cid: Option<String>,
    val: String,
    #[serde(default)]
    neg: bool,
    cts: String,
    #[serde(default)]
    exp: Option<String>,
    sig: SignatureOwned,
}

#[derive(Deserialize)]
struct SignatureOwned {
    #[serde(rename = "$bytes")]
    bytes: String,
}

impl<'de> Deserialize<'de> for SignedLabel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireOwned::deserialize(deserializer)?;
        if wire.ver != 1 {
            return Err(DeError::custom("only label version 1 is supported"));
        }
        let bytes = STANDARD_NO_PAD
            .decode(wire.sig.bytes.trim_end_matches('='))
            .map_err(|error| DeError::custom(format!("invalid label signature: {error}")))?;
        let sig: [u8; 64] = bytes
            .try_into()
            .map_err(|_| DeError::custom("a label signature must be 64 bytes"))?;
        Ok(Self {
            label: Label {
                ver: wire.ver,
                src: wire.src,
                uri: wire.uri,
                cid: wire.cid,
                val: wire.val,
                neg: wire.neg,
                cts: wire.cts,
                exp: wire.exp,
            },
            sig,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Label {
        Label {
            ver: 1,
            src: "did:plc:labeler".into(),
            uri: "did:plc:alice".into(),
            cid: None,
            val: "online".into(),
            neg: false,
            cts: "2026-11-14T00:00:00.000Z".into(),
            exp: None,
        }
    }

    #[test]
    fn serialises_the_lexicon_field_names_and_omits_absent_optionals() {
        let label = sample();
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

    #[test]
    fn signed_labels_use_dag_json_bytes_and_round_trip() {
        let signed = SignedLabel::new(sample(), [7u8; 64]);
        let value = serde_json::to_value(&signed).unwrap();
        assert_eq!(
            value["sig"]["$bytes"],
            STANDARD_NO_PAD.encode([7u8; 64]).as_str()
        );
        assert!(value.get("seq").is_none());
        let back: SignedLabel = serde_json::from_value(value).unwrap();
        assert_eq!(back, signed);
    }

    #[test]
    fn expiry_uses_rfc3339_and_fails_open() {
        let mut label = sample();
        label.exp = Some("2026-11-14T00:00:00.000Z".into());
        let expiry = super::super::time::parse_rfc3339(label.exp.as_deref().unwrap()).unwrap();
        assert!(!label.is_expired(expiry as u64));
        assert!(label.is_expired(expiry as u64 + 1));
        label.exp = Some("not a timestamp".into());
        assert!(!label.is_expired(u64::MAX));
    }
}
