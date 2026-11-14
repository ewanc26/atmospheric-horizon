//! Canonical DRISL/DAG-CBOR encoding of an unsigned label.
//!
//! Only the subset a label needs is implemented: definite-length maps, text,
//! unsigned integers and booleans. Map keys are sorted length-first, then by
//! unsigned bytes, matching the label signing format. The `sig` field is not
//! part of the signing input, so byte strings are not needed here.

use super::label::Label;

/// A value in the label map. `cid`/`exp`/`src`/`uri`/`val` are text, `neg` is
/// a boolean and `ver` is an unsigned integer.
enum Value<'a> {
    Text(&'a str),
    Unsigned(u64),
    Bool(bool),
}

/// Encode the label exactly as it is signed: `cts`, optional `cid` and `exp`,
/// `neg` only when true, `src`, `uri`, `val` and `ver`, in canonical key order.
pub(super) fn encode_label(label: &Label) -> Vec<u8> {
    let mut entries: Vec<(&str, Value<'_>)> = Vec::with_capacity(8);
    entries.push(("cts", Value::Text(&label.cts)));
    if let Some(cid) = &label.cid {
        entries.push(("cid", Value::Text(cid)));
    }
    if let Some(exp) = &label.exp {
        entries.push(("exp", Value::Text(exp)));
    }
    if label.neg {
        entries.push(("neg", Value::Bool(true)));
    }
    entries.push(("src", Value::Text(&label.src)));
    entries.push(("uri", Value::Text(&label.uri)));
    entries.push(("val", Value::Text(&label.val)));
    entries.push(("ver", Value::Unsigned(u64::from(label.ver))));
    entries.sort_by(|left, right| canonical_key(left.0, right.0));

    let mut out = Vec::new();
    encode_arg(5, entries.len() as u64, &mut out);
    for (key, value) in entries {
        encode_text(key, &mut out);
        match value {
            Value::Text(text) => encode_text(text, &mut out),
            Value::Unsigned(number) => encode_arg(0, number, &mut out),
            Value::Bool(value) => out.push(if value { 0xf5 } else { 0xf4 }),
        }
    }
    out
}

/// Longest keys first would be plain lexicographic; DAG-CBOR instead sorts by
/// UTF-8 length, then by unsigned byte value.
fn canonical_key(left: &str, right: &str) -> std::cmp::Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

fn encode_text(value: &str, out: &mut Vec<u8>) {
    encode_arg(3, value.len() as u64, out);
    out.extend_from_slice(value.as_bytes());
}

/// Encode a major type and its argument in the shortest CBOR head form.
fn encode_arg(major: u8, value: u64, out: &mut Vec<u8>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Label {
        Label {
            ver: 1,
            src: "did:plc:abc".into(),
            uri: "did:plc:def".into(),
            cid: None,
            val: "online".into(),
            neg: false,
            cts: "2026-11-14T00:00:00.000Z".into(),
            exp: None,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    #[test]
    fn minimal_label_matches_the_canonical_bytes() {
        // a5 + cts, src, uri, val, ver (all three-byte keys, bytewise order).
        let expected = concat!(
            "a5",
            "63637473",
            "7818",
            "323032362d31312d31345430303a30303a30302e3030305a",
            "63737263",
            "6b",
            "6469643a706c633a616263",
            "63757269",
            "6b",
            "6469643a706c633a646566",
            "6376616c",
            "66",
            "6f6e6c696e65",
            "63766572",
            "01",
        );
        assert_eq!(hex(&encode_label(&sample())), expected);
    }

    #[test]
    fn optionals_are_encoded_in_length_first_order() {
        let mut label = sample();
        label.cid = Some("bafkreiexample".into());
        label.exp = Some("2026-12-14T00:00:00.000Z".into());
        label.neg = true;
        let encoded = hex(&encode_label(&label));

        assert!(encoded.starts_with("a8636369646e"), "{encoded}");
        assert!(encoded.contains("636e6567f5"), "neg is true: {encoded}");
        assert!(encoded.ends_with("6376657201"));

        // All eight keys are three bytes, so the order is bytewise.
        let keys = [
            "63636964", "63637473", "63657870", "636e6567", "63737263", "63757269", "6376616c",
            "63766572",
        ];
        let mut previous = 0;
        for key in keys {
            let position = encoded
                .find(key)
                .unwrap_or_else(|| panic!("{key} in {encoded}"));
            assert!(position >= previous, "{key} is out of order: {encoded}");
            previous = position;
        }
    }
}
