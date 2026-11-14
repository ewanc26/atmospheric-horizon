//! Labeler issuance primitives (M56, the first slice of #112).
//!
//! atpcraft signs labels with secp256k1 over the label's canonical DAG-CBOR
//! form and publishes the key as a `did:key` `#atproto_label` verification
//! method. This scope provides that signing and encoding layer. The HTTP
//! endpoints, PLC update, declaration record, label store and game triggers
//! remain in #112.
//!
//! Wire shapes come from the lexicons/spec only — no atpcraft code is copied.

mod cbor;
mod label;
mod signer;

pub use label::Label;
pub use signer::{LabelError, LabelSigner};
