//! Labeler issuance primitives (M56–M57, part of #112).
//!
//! atpcraft signs labels with secp256k1 over the label's canonical DAG-CBOR
//! form and publishes the key as a `did:key` `#atproto_label` verification
//! method. M56 provides that signing and encoding layer; M57 adds the durable
//! label log, active-state index, key persistence and `queryLabels` support;
//! M58 adds the live `subscribeLabels` stream.
//!
//! The declaration record, PLC update and game triggers remain in #112. Wire
//! shapes come from the lexicons/spec only — no atpcraft code is copied.

mod cbor;
mod key;
mod label;
mod log;
mod query;
mod service;
mod signer;
mod state;
mod stream;
mod time;

pub use label::{Label, SignedLabel};
pub use log::LogEntry;
pub use query::Query;
pub use service::{EmitLabel, Labeler, LabelerError, QueryResult};
pub use signer::{LabelError, LabelSigner};
pub(crate) use stream::{error_frame, labels_frame};
