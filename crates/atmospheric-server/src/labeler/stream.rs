//! `com.atproto.label.subscribeLabels` wire frames.
//!
//! Each binary WebSocket message is two concatenated canonical DAG-CBOR
//! objects, a header and a body:
//!
//! ```text
//! #labels: {t: "#labels", op: 1} + {seq, labels: [label, …]}
//! error:   {op: -1} + {error, message?}
//! ```
//!
//! A body can carry a batch, but this service emits one label per frame. The
//! signature is a CBOR byte string, unlike its DAG-JSON `{"$bytes": …}` shape
//! in `queryLabels` and the on-disk log.

use super::{
    cbor::{Value, encode_map},
    label::SignedLabel,
    log::LogEntry,
};

/// `FrameType.Message` in the reference XRPC stream framing.
const OP_MESSAGE: i64 = 1;

/// `FrameType.Error` in the reference XRPC stream framing.
const OP_ERROR: i64 = -1;

/// Encode one sequenced label as a binary `#labels` frame.
pub(crate) fn labels_frame(entry: &LogEntry) -> Vec<u8> {
    let header = encode_map(vec![
        ("op", Value::Signed(OP_MESSAGE)),
        ("t", Value::Text("#labels")),
    ]);
    let labels = Value::Array(vec![Value::Map(label_entries(&entry.label))]);
    let body = encode_map(vec![
        ("seq", Value::Unsigned(entry.seq)),
        ("labels", labels),
    ]);

    let mut frame = header;
    frame.extend_from_slice(&body);
    frame
}

/// Encode an error frame. The caller closes the socket afterwards.
pub(crate) fn error_frame(error: &str, message: Option<&str>) -> Vec<u8> {
    let header = encode_map(vec![("op", Value::Signed(OP_ERROR))]);
    let mut entries = vec![("error", Value::Text(error))];
    if let Some(message) = message {
        entries.push(("message", Value::Text(message)));
    }
    let body = encode_map(entries);

    let mut frame = header;
    frame.extend_from_slice(&body);
    frame
}

fn label_entries(signed: &SignedLabel) -> Vec<(&str, Value<'_>)> {
    let label = &signed.label;
    let mut entries = Vec::with_capacity(9);
    entries.push(("ver", Value::Unsigned(u64::from(label.ver))));
    entries.push(("src", Value::Text(&label.src)));
    entries.push(("uri", Value::Text(&label.uri)));
    if let Some(cid) = &label.cid {
        entries.push(("cid", Value::Text(cid)));
    }
    entries.push(("val", Value::Text(&label.val)));
    if label.neg {
        entries.push(("neg", Value::Bool(true)));
    }
    entries.push(("cts", Value::Text(&label.cts)));
    if let Some(exp) = &label.exp {
        entries.push(("exp", Value::Text(exp)));
    }
    entries.push(("sig", Value::Bytes(&signed.sig)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labeler::Label;

    fn entry() -> LogEntry {
        LogEntry {
            seq: 7,
            label: SignedLabel::new(
                Label {
                    ver: 1,
                    src: "did:plc:labeler".into(),
                    uri: "did:plc:alice".into(),
                    cid: None,
                    val: "online".into(),
                    neg: false,
                    cts: "2026-11-14T00:00:00.000Z".into(),
                    exp: None,
                },
                [7u8; 64],
            ),
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
    fn labels_frame_starts_with_the_canonical_header_and_body() {
        let encoded = hex(&labels_frame(&entry()));
        assert!(
            encoded.starts_with(concat!(
                "a2617467236c6162656c73626f7001",
                "a2",
                "63736571",
                "07",
                "666c6162656c73",
                "81",
                "a6",
                "63637473",
                "7818",
                "323032362d31312d31345430303a30303a30302e3030305a",
            )),
            "{encoded}"
        );
        assert!(
            encoded.contains("637369675840070707"),
            "sig is a 64-byte string: {encoded}"
        );
        assert!(encoded.ends_with("6376657201"), "{encoded}");
    }

    #[test]
    fn error_frame_is_an_op_minus_one_header_and_error_body() {
        assert_eq!(
            hex(&error_frame("FutureCursor", None)),
            concat!(
                "a1626f7020",
                "a1",
                "656572726f72",
                "6c",
                "467574757265437572736f72",
            )
        );

        let encoded = hex(&error_frame("FutureCursor", Some("past the tail")));
        assert!(encoded.starts_with("a1626f7020a2656572726f72"), "{encoded}");
        assert!(encoded.contains("676d657373616765"), "{encoded}");
    }
}
