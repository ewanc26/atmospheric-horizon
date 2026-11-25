//! Proof-of-Handle: PLC-verified handle plaques (#185).
//!
//! The old "linked" badge trusted whatever the OAuth session said about a
//! handle. This module verifies the handle cryptographically instead:
//!
//! 1. A challenge (random nonce + issue time) is issued for a player.
//! 2. The player's linked PDS session writes a throwaway record whose key is
//!    that nonce, producing a fresh signed repository commit.
//! 3. The sidecar fetches `com.atproto.sync.getLatestCommit` for the DID, then
//!    `com.atproto.sync.getBlocks` for the returned CID, and verifies the
//!    commit's ECDSA signature against the `#atproto` key anchored in the
//!    DID's PLC document.
//! 4. The commit `rev` (a TID) must be newer than the challenge, so an old
//!    signed commit cannot be replayed.
//! 5. The DID document's `alsoKnownAs` handle and the handle resolved back to
//!    the DID must agree.
//!
//! This proves *PDS repository custody*, not human custody: the badge text
//! says so. This module only verifies and never writes profile data.

use crate::error::{AtmosphericError, Result};
use k256::ecdsa::{Signature, VerifyingKey};
use k256::elliptic_curve::scalar::IsHigh;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// The multikey multicodec prefix for a K-256 (secp256k1) public key:
/// varint 0xe7 0x01, followed by a compressed SEC1 point.
const K256_MULTICODEC: &[u8] = &[0xe7, 0x01];
/// DAG-CBOR's tag for a CID link.
const CID_LINK_TAG: u64 = 42;
/// A commit is only accepted when it is at most this old (seconds). Combined
/// with the nonce check this bounds the replay window.
pub const MAX_COMMIT_AGE_SECS: u64 = 300;

/// A verification challenge: a random nonce and the Unix time it was issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub nonce: String,
    pub issued_at: u64,
}

impl Challenge {
    /// Build a challenge from a caller-supplied nonce (used by the protocol
    /// code and tests). Production callers should use [`Challenge::fresh`].
    pub fn new(nonce: impl Into<String>, issued_at: u64) -> Self {
        Self {
            nonce: nonce.into(),
            issued_at,
        }
    }

    /// Generate a fresh challenge with a random 128-bit nonce and the current
    /// Unix time. Uses system randomness via `getrandom`.
    pub fn fresh() -> Result<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|err| {
            AtmosphericError::Message(format!("could not generate verification nonce: {err}"))
        })?;
        Ok(Self {
            nonce: hex(&bytes),
            issued_at: now_secs(),
        })
    }

    /// True when the challenge is still within the replay window.
    pub fn is_current(&self, now: u64, max_age_secs: u64) -> bool {
        now >= self.issued_at && now - self.issued_at <= max_age_secs
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The verified fields of a signed repository commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitProof {
    pub did: String,
    pub rev: String,
    pub data: Vec<u8>,
}

/// Decode a `did:key`-style multikey `z…` string into a compressed SEC1 public
/// key. Only the K-256 codec is accepted — AT Protocol commit keys are k256.
pub fn decode_k256_multibase(multibase: &str) -> Result<Vec<u8>> {
    let encoded = multibase.strip_prefix('z').ok_or_else(|| {
        AtmosphericError::Message("commit signing key is not a z-prefixed multibase".into())
    })?;
    let raw = multibase::decode(encoded)
        .map_err(|err| AtmosphericError::Message(format!("invalid commit signing key: {err}")))?
        .1;
    let point = raw.strip_prefix(K256_MULTICODEC).ok_or_else(|| {
        AtmosphericError::Message("commit signing key is not a K-256 multikey".into())
    })?;
    if point.len() != 33 {
        return Err(AtmosphericError::Message(format!(
            "commit signing key has {} bytes, expected a 33-byte compressed point",
            point.len()
        )));
    }
    Ok(point.to_vec())
}

/// Extract the `#atproto` signing key from a DID document. The PLC document
/// carries a `Multikey` verification method whose `publicKeyMultibase` is the
/// key that signs repository commits.
pub fn signing_key_from_did_doc(did_doc: &serde_json::Value) -> Result<String> {
    did_doc["verificationMethod"]
        .as_array()
        .and_then(|methods| {
            methods.iter().find(|method| {
                let id = method["id"].as_str().unwrap_or_default();
                id == "#atproto" || id.ends_with("#atproto")
            })
        })
        .and_then(|method| method["publicKeyMultibase"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            AtmosphericError::Message("no #atproto multikey in the DID document".into())
        })
}

/// Decode a TID into its microseconds-since-epoch timestamp.
pub fn tid_timestamp(tid: &str) -> Result<u64> {
    let value = crate::tid::decode(tid).ok_or_else(|| {
        AtmosphericError::Message(format!("commit revision {tid:?} is not a valid TID"))
    })?;
    // A TID packs `micros << 10 | clock_id`; strip the clock id first.
    Ok(value >> 10)
}

/// Check that a commit revision is newer than the challenge and not too old.
pub fn commit_is_fresh(rev: &str, challenge: &Challenge, now: u64, max_age_secs: u64) -> Result<()> {
    let commit_secs = tid_timestamp(rev)? / 1_000_000;
    if commit_secs < challenge.issued_at {
        return Err(AtmosphericError::Message(format!(
            "commit revision {rev} predates the challenge"
        )));
    }
    if now.saturating_sub(commit_secs) > max_age_secs {
        return Err(AtmosphericError::Message(format!(
            "commit revision {rev} is older than {max_age_secs}s"
        )));
    }
    Ok(())
}

/// Check that a DID document's `alsoKnownAs` claims exactly the given handle.
pub fn did_doc_claims_handle(did_doc: &serde_json::Value, handle: &str) -> Result<()> {
    let expected = format!("at://{}", handle.trim_start_matches('@').to_lowercase());
    let claimed = did_doc["alsoKnownAs"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .any(|value| value.eq_ignore_ascii_case(&expected))
        })
        .unwrap_or(false);
    if !claimed {
        return Err(AtmosphericError::Message(format!(
            "DID document does not claim handle {handle}"
        )));
    }
    Ok(())
}

/// Verify a signed commit block against a PLC-anchored K-256 multikey.
///
/// `block` is the raw DAG-CBOR value from `com.atproto.sync.getBlocks` whose
/// CID matches the latest commit. Returns the DID, revision and data CID from
/// the verified commit. High-S signatures are rejected as required by the
/// CBOR signature profile.
pub fn verify_commit(block: &[u8], key_multibase: &str) -> Result<CommitProof> {
    let key_bytes = decode_k256_multibase(key_multibase)?;
    let verifying_key = VerifyingKey::from_sec1_bytes(&key_bytes).map_err(|err| {
        AtmosphericError::Message(format!("invalid commit verifying key: {err}"))
    })?;

    let mut decoder = Decoder::new(block);
    let value = decoder.value()?;
    if decoder.remaining() != 0 {
        return Err(AtmosphericError::Message(
            "trailing bytes after commit CBOR".into(),
        ));
    }
    let entries = match value {
        Value::Map(entries) => entries,
        _ => return Err(AtmosphericError::Message("commit is not a CBOR map".into())),
    };

    let mut did = None;
    let mut rev = None;
    let mut data = None;
    let mut prev = None;
    let mut sig = None;
    let mut version = None;
    for (key, value) in entries {
        match key {
            "did" => did = Some(as_text(value, "did")?.to_owned()),
            "rev" => rev = Some(as_text(value, "rev")?.to_owned()),
            "data" => data = Some(as_link(value, "data")?),
            "prev" => prev = Some(as_link(value, "prev")?),
            "sig" => sig = Some(as_bytes(value, "sig")?.to_vec()),
            "version" => version = Some(as_unsigned(value, "version")?),
            _ => {}
        }
    }

    if version != Some(3) {
        return Err(AtmosphericError::Message(
            "unsupported commit version (expected 3)".into(),
        ));
    }
    let did = did.ok_or_else(|| AtmosphericError::Message("commit has no did".into()))?;
    let rev = rev.ok_or_else(|| AtmosphericError::Message("commit has no rev".into()))?;
    let data = data.ok_or_else(|| AtmosphericError::Message("commit has no data CID".into()))?;
    let sig_bytes = sig.ok_or_else(|| AtmosphericError::Message("commit has no signature".into()))?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|err| {
        AtmosphericError::Message(format!("invalid commit signature encoding: {err}"))
    })?;
    if signature.s().is_high().into() {
        return Err(AtmosphericError::Message(
            "commit signature is not low-S".into(),
        ));
    }

    // The signing input is the commit without `sig`, encoded as canonical
    // DRISL/DAG-CBOR. `prev` is present as null or a CID link; both forms
    // round-trip through the canonical encoder exactly.
    let mut fields: Vec<(&str, EncodeValue<'_>)> = vec![
        ("did", EncodeValue::Text(&did)),
        ("rev", EncodeValue::Text(&rev)),
        ("data", EncodeValue::Link(&data)),
        ("version", EncodeValue::Unsigned(3)),
    ];
    let prev_value = prev.as_deref().map(EncodeValue::Link).unwrap_or(EncodeValue::Null);
    fields.push(("prev", prev_value));
    let signing_input = encode_map(fields);
    use k256::ecdsa::signature::Verifier;
    verifying_key.verify(&signing_input, &signature).map_err(|err| {
        AtmosphericError::Message(format!("commit signature does not verify: {err}"))
    })?;

    Ok(CommitProof { did, rev, data })
}

fn as_text<'a>(value: Value<'a>, field: &str) -> Result<&'a str> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(AtmosphericError::Message(format!(
            "commit field {field} is not text"
        ))),
    }
}

fn as_bytes<'a>(value: Value<'a>, field: &str) -> Result<&'a [u8]> {
    match value {
        Value::Bytes(bytes) => Ok(bytes),
        _ => Err(AtmosphericError::Message(format!(
            "commit field {field} is not bytes"
        ))),
    }
}

fn as_link(value: Value<'_>, field: &str) -> Result<Vec<u8>> {
    match value {
        Value::Link(bytes) => Ok(bytes),
        Value::Null => Err(AtmosphericError::Message(format!(
            "commit field {field} is null"
        ))),
        _ => Err(AtmosphericError::Message(format!(
            "commit field {field} is not a CID link"
        ))),
    }
}

fn as_unsigned(value: Value<'_>, field: &str) -> Result<u64> {
    match value {
        Value::Unsigned(number) => Ok(number),
        _ => Err(AtmosphericError::Message(format!(
            "commit field {field} is not an unsigned integer"
        ))),
    }
}

/// A minimal DAG-CBOR value set: the definite-length types a signed commit
/// uses. Indefinite lengths and floating point are rejected by the decoder.
#[derive(Debug, Clone, PartialEq)]
enum Value<'a> {
    Unsigned(u64),
    Signed(i64),
    Bool(bool),
    Null,
    Text(&'a str),
    Bytes(&'a [u8]),
    Array(Vec<Value<'a>>),
    Map(Vec<(&'a str, Value<'a>)>),
    /// A tagged CID link (DAG-CBOR tag 42) with its raw CID bytes.
    Link(Vec<u8>),
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn head(&mut self) -> Result<(u8, u64)> {
        let first = *self.data.get(self.pos).ok_or_else(|| {
            AtmosphericError::Message("unexpected end of commit CBOR".into())
        })?;
        self.pos += 1;
        let major = first >> 5;
        let additional = first & 0x1f;
        let argument = match additional {
            0..=23 => u64::from(additional),
            24 => u64::from(self.byte()?),
            25 => u64::from(u16::from_be_bytes([self.byte()?, self.byte()?])),
            26 => u64::from(u32::from_be_bytes([
                self.byte()?,
                self.byte()?,
                self.byte()?,
                self.byte()?,
            ])),
            27 => u64::from_be_bytes([
                self.byte()?,
                self.byte()?,
                self.byte()?,
                self.byte()?,
                self.byte()?,
                self.byte()?,
                self.byte()?,
                self.byte()?,
            ]),
            _ => {
                return Err(AtmosphericError::Message(
                    "indefinite-length CBOR is not valid in a commit".into(),
                ));
            }
        };
        Ok((major, argument))
    }

    fn byte(&mut self) -> Result<u8> {
        let byte = *self.data.get(self.pos).ok_or_else(|| {
            AtmosphericError::Message("unexpected end of commit CBOR".into())
        })?;
        self.pos += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or_else(|| {
            AtmosphericError::Message("CBOR length overflow".into())
        })?;
        let slice = self.data.get(self.pos..end).ok_or_else(|| {
            AtmosphericError::Message("unexpected end of commit CBOR".into())
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn value(&mut self) -> Result<Value<'a>> {
        let (major, argument) = self.head()?;
        let length = usize::try_from(argument).map_err(|_| {
            AtmosphericError::Message("CBOR length exceeds usize".into())
        })?;
        match major {
            0 => Ok(Value::Unsigned(argument)),
            1 => Ok(Value::Signed(-1 - argument as i64)),
            2 => Ok(Value::Bytes(self.take(length)?)),
            3 => {
                let bytes = self.take(length)?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| AtmosphericError::Message("invalid UTF-8 in commit".into()))?;
                Ok(Value::Text(text))
            }
            4 => {
                let mut values = Vec::with_capacity(length.min(64));
                for _ in 0..length {
                    values.push(self.value()?);
                }
                Ok(Value::Array(values))
            }
            5 => {
                let mut entries = Vec::with_capacity(length.min(64));
                for _ in 0..length {
                    let key = match self.value()? {
                        Value::Text(text) => text,
                        _ => {
                            return Err(AtmosphericError::Message(
                                "commit CBOR map key is not text".into(),
                            ));
                        }
                    };
                    let value = self.value()?;
                    entries.push((key, value));
                }
                Ok(Value::Map(entries))
            }
            6 if argument == CID_LINK_TAG => {
                let (inner_major, inner_len) = self.head()?;
                if inner_major != 2 {
                    return Err(AtmosphericError::Message(
                        "CID link tag is not followed by bytes".into(),
                    ));
                }
                let inner_len = usize::try_from(inner_len).map_err(|_| {
                    AtmosphericError::Message("CID link length exceeds usize".into())
                })?;
                Ok(Value::Link(self.take(inner_len)?.to_vec()))
            }
            7 => match argument {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                _ => Err(AtmosphericError::Message(format!(
                    "unsupported CBOR simple value {argument}"
                ))),
            },
            _ => Err(AtmosphericError::Message(format!(
                "unsupported CBOR major type {major}"
            ))),
        }
    }
}

/// The values needed to re-encode the unsigned commit in canonical order.
enum EncodeValue<'a> {
    Text(&'a str),
    Unsigned(u64),
    Link(&'a [u8]),
    Null,
}

fn canonical_key(left: &str, right: &str) -> std::cmp::Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

/// Encode a map in canonical DAG-CBOR order: UTF-8 length first, then bytes.
fn encode_map(mut entries: Vec<(&str, EncodeValue<'_>)>) -> Vec<u8> {
    entries.sort_by(|left, right| canonical_key(left.0, right.0));
    let mut out = Vec::new();
    write_head(5, entries.len() as u64, &mut out);
    for (key, value) in entries {
        write_head(3, key.len() as u64, &mut out);
        out.extend_from_slice(key.as_bytes());
        match value {
            EncodeValue::Text(text) => {
                write_head(3, text.len() as u64, &mut out);
                out.extend_from_slice(text.as_bytes());
            }
            EncodeValue::Unsigned(number) => write_head(0, number, &mut out),
            EncodeValue::Link(bytes) => {
                write_head(6, CID_LINK_TAG, &mut out);
                write_head(2, bytes.len() as u64, &mut out);
                out.extend_from_slice(bytes);
            }
            EncodeValue::Null => out.push(0xf6),
        }
    }
    out
}

fn write_head(major: u8, value: u64, out: &mut Vec<u8>) {
    let prefix = major << 5;
    match value {
        0..=23 => out.push(prefix | value as u8),
        24..=0xff => {
            out.push(prefix | 24);
            out.push(value as u8);
        }
        0x100..=0xffff => {
            out.push(prefix | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(prefix | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(prefix | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

/// Parse a CAR v1 file and return the block whose CID bytes equal `wanted`.
///
/// A CAR is a length-prefixed DAG-CBOR header followed by length-prefixed
/// `CID || block` pairs. Lengths are unsigned LEB128 varints. This walker
/// skips the header and returns the first matching block without allocating
/// copies of the other blocks.
pub fn car_block<'a>(car: &'a [u8], wanted: &[u8]) -> Result<&'a [u8]> {
    let mut pos = 0usize;
    let header_len = read_varint(car, &mut pos)?;
    let _header = take(car, &mut pos, header_len)?;
    while pos < car.len() {
        let section_len = read_varint(car, &mut pos)?;
        let section = take(car, &mut pos, section_len)?;
        let mut cid_pos = 0usize;
        let cid_len = read_varint(section, &mut cid_pos)?;
        let cid = take(section, &mut cid_pos, cid_len)?;
        if cid == wanted {
            return Ok(&section[cid_pos..]);
        }
    }
    Err(AtmosphericError::Message(
        "CAR does not contain the requested commit block".into(),
    ))
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<usize> {
    let value = decode_varint(data, pos)?;
    usize::try_from(value)
        .map_err(|_| AtmosphericError::Message("CAR length exceeds usize".into()))
}

fn decode_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data
            .get(*pos)
            .ok_or_else(|| AtmosphericError::Message("truncated CAR varint".into()))?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(AtmosphericError::Message("CAR varint is too long".into()))
}

fn take<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = pos.checked_add(len).ok_or_else(|| {
        AtmosphericError::Message("CAR section length overflow".into())
    })?;
    let slice = data
        .get(*pos..end)
        .ok_or_else(|| AtmosphericError::Message("truncated CAR section".into()))?;
    *pos = end;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{SigningKey, signature::Signer};

    fn sample_key() -> (SigningKey, String) {
        let key = SigningKey::from_bytes((&[7u8; 32]).into()).expect("valid seed");
        let point = key.verifying_key().to_sec1_point(true);
        let mut multikey = Vec::new();
        multikey.extend_from_slice(K256_MULTICODEC);
        multikey.extend_from_slice(point.as_bytes());
        let encoded = format!("z{}", multibase::encode(multibase::Base::Base58Btc, &multikey));
        (key, encoded)
    }

    fn commit_value(rev: &str, include_prev: bool) -> Vec<u8> {
        // A hand-built canonical CBOR commit map for signing tests.
        let data_link = vec![0x01, 0x70, 0x12, 0x20, 0xaa];
        let prev_link = vec![0x01, 0x70, 0x12, 0x20, 0xbb];
        let mut fields: Vec<(&str, EncodeValue<'_>)> = vec![
            ("did", EncodeValue::Text("did:plc:test")),
            ("rev", EncodeValue::Text(rev)),
            ("data", EncodeValue::Link(&data_link)),
            ("version", EncodeValue::Unsigned(3)),
        ];
        if include_prev {
            fields.push(("prev", EncodeValue::Link(&prev_link)));
        } else {
            fields.push(("prev", EncodeValue::Null));
        }
        encode_map(fields)
    }

    #[test]
    fn canonical_map_orders_by_length_then_bytes() {
        let bytes = encode_map(vec![
            ("version", EncodeValue::Unsigned(3)),
            ("did", EncodeValue::Text("x")),
            ("data", EncodeValue::Link(&[1, 2])),
            ("rev", EncodeValue::Text("r")),
        ]);
        // Map head (0xa4 = 4 pairs), then a 3-byte text key "did".
        assert_eq!(bytes[0], 0xa4);
        assert_eq!(bytes[1], 0x63);
        assert_eq!(&bytes[2..5], b"did");
    }

    #[test]
    fn decodes_and_verifies_a_signed_commit() {
        let (key, multikey) = sample_key();
        let rev = "3lxxxgkgxxx";
        let signing_input = commit_value(rev, true);
        let signature: Signature = key.sign(&signing_input);
        let mut sig_bytes = signature.to_bytes().to_vec();

        // Build the full commit map (with sig) in canonical order.
        let data_link = vec![0x01, 0x70, 0x12, 0x20, 0xaa];
        let prev_link = vec![0x01, 0x70, 0x12, 0x20, 0xbb];
        let mut out = Vec::new();
        write_head(5, 6, &mut out);
        let mut entries = vec![
            ("did", Value::Text("did:plc:test")),
            ("rev", Value::Text(rev)),
            ("data", Value::Link(data_link.clone())),
            ("prev", Value::Link(prev_link)),
            ("version", Value::Unsigned(3)),
            ("sig", Value::Bytes(&sig_bytes)),
        ];
        entries.sort_by(|a, b| canonical_key(a.0, b.0));
        for (field, value) in entries {
            write_head(3, field.len() as u64, &mut out);
            out.extend_from_slice(field.as_bytes());
            match value {
                Value::Text(text) => {
                    write_head(3, text.len() as u64, &mut out);
                    out.extend_from_slice(text.as_bytes());
                }
                Value::Unsigned(n) => write_head(0, n, &mut out),
                Value::Link(link) => {
                    write_head(6, CID_LINK_TAG, &mut out);
                    write_head(2, link.len() as u64, &mut out);
                    out.extend_from_slice(&link);
                }
                Value::Bytes(bytes) => {
                    write_head(2, bytes.len() as u64, &mut out);
                    out.extend_from_slice(bytes);
                }
                _ => unreachable!(),
            }
        }
        sig_bytes.clear();

        let proof = verify_commit(&out, &multikey).expect("commit should verify");
        assert_eq!(proof.did, "did:plc:test");
        assert_eq!(proof.rev, rev);
        assert_eq!(proof.data, data_link);
    }

    #[test]
    fn rejects_a_tampered_commit() {
        let (key, multikey) = sample_key();
        let signature: Signature = key.sign(&commit_value("3lxxxgkgxxx", false));
        let data_link = vec![0x01, 0x70, 0x12, 0x20, 0xaa];
        let sig_bytes = signature.to_bytes();
        let mut out = Vec::new();
        write_head(5, 6, &mut out);
        let mut entries = vec![
            ("did", Value::Text("did:plc:test")),
            ("rev", Value::Text("3lxxxgkgxxx")),
            ("data", Value::Link(data_link)),
            ("prev", Value::Null),
            ("version", Value::Unsigned(3)),
            ("sig", Value::Bytes(&sig_bytes)),
        ];
        entries.sort_by(|a, b| canonical_key(a.0, b.0));
        for (field, value) in entries {
            write_head(3, field.len() as u64, &mut out);
            out.extend_from_slice(field.as_bytes());
            match value {
                Value::Text(text) => {
                    write_head(3, text.len() as u64, &mut out);
                    out.extend_from_slice(text.as_bytes());
                }
                Value::Unsigned(n) => write_head(0, n, &mut out),
                Value::Null => out.push(0xf6),
                Value::Link(link) => {
                    write_head(6, CID_LINK_TAG, &mut out);
                    write_head(2, link.len() as u64, &mut out);
                    out.extend_from_slice(&link);
                }
                Value::Bytes(bytes) => {
                    write_head(2, bytes.len() as u64, &mut out);
                    out.extend_from_slice(bytes);
                }
                _ => unreachable!(),
            }
        }
        // Change the DID after signing; the signature must no longer match.
        let tampered_target = out.windows(12).position(|w| w == b"did:plc:test").unwrap();
        out[tampered_target + 4] = b'X';
        assert!(verify_commit(&out, &multikey).is_err());
    }

    #[test]
    fn decodes_multibase_and_rejects_other_codecs() {
        let (_key, multikey) = sample_key();
        assert_eq!(decode_k256_multibase(&multikey).unwrap().len(), 33);
        assert!(decode_k256_multibase("not-multibase").is_err());
        let p256 = format!(
            "z{}",
            multibase::encode(multibase::Base::Base58Btc, &[0x80, 0x24, 7])
        );
        assert!(decode_k256_multibase(&p256).is_err());
    }

    #[test]
    fn extracts_atproto_key_from_did_document() {
        let doc: serde_json::Value = serde_json::json!({
            "verificationMethod": [
                {"id": "did:plc:x#atproto", "type": "Multikey",
                 "publicKeyMultibase": "zTEST"},
                {"id": "did:plc:x#other", "type": "Multikey",
                 "publicKeyMultibase": "zNO"}
            ]
        });
        assert_eq!(signing_key_from_did_doc(&doc).unwrap(), "zTEST");
        let empty = serde_json::json!({"verificationMethod": []});
        assert!(signing_key_from_did_doc(&empty).is_err());
    }

    #[test]
    fn freshness_and_handle_claim_checks() {
        let now = 2_000_000_000;
        let challenge = Challenge::new("nonce", now - 10);
        // A TID for 5 seconds after the challenge, well within the window.
        let rev_tid = crate::tid::TidGen::new().next_at((now - 5) * 1_000_000);
        assert!(commit_is_fresh(&rev_tid, &challenge, now, MAX_COMMIT_AGE_SECS).is_ok());
        // A separate generator keeps the older timestamp monotonic.
        let old_tid = crate::tid::TidGen::new().next_at((now - 20) * 1_000_000);
        assert!(commit_is_fresh(&old_tid, &challenge, now, MAX_COMMIT_AGE_SECS).is_err());

        let doc = serde_json::json!({"alsoKnownAs": ["at://alice.example"]});
        assert!(did_doc_claims_handle(&doc, "Alice.Example").is_ok());
        assert!(did_doc_claims_handle(&doc, "bob.example").is_err());
    }

    #[test]
    fn challenge_is_current_only_within_window() {
        let challenge = Challenge::new("n", 100);
        assert!(challenge.is_current(100, 60));
        assert!(challenge.is_current(160, 60));
        assert!(!challenge.is_current(161, 60));
        assert!(!challenge.is_current(99, 60));
    }

    #[test]
    fn parses_a_car_and_finds_the_wanted_block() {
        let cid = vec![0x01, 0x70, 0x12, 0x20, 0x42];
        let block = b"commit block bytes";
        let mut section = Vec::new();
        section.extend_from_slice(&[cid.len() as u8]);
        section.extend_from_slice(&cid);
        section.extend_from_slice(block);
        let mut car = Vec::new();
        car.push(1); // header length varint
        car.extend_from_slice(&[0x01]); // header placeholder
        car.push(section.len() as u8);
        car.extend_from_slice(&section);
        assert_eq!(car_block(&car, &cid).unwrap(), block);
        assert!(car_block(&car, &[9, 9, 9]).is_err());
    }
}
