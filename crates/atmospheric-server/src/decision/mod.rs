//! Ledger of Decisions sidecar state (#199).
//!
//! The ledger keeps the owner-only signing key and subject pepper under the
//! data directory, mirrors signed records in memory for public reads, and
//! appends every record to a durable JSONL file. It does not open a socket.

mod ledger;

pub use ledger::{
    DecisionInput, DecisionLedger, DecisionSettings, LedgerStatus, MAX_LIMIT, now_rfc3339,
};
