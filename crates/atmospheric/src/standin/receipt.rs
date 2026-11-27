//! Signed audit receipts for Stand-In decisions (#206).
//!
//! Every authorization attempt — allowed or denied — gets a receipt signed by
//! the doorkeeper's own key, so a player can later review what was asked in
//! their name. A receipt names the owner the doorkeeper guards, the agent DID
//! from the grant (only once that grant verified with the owner's key; an
//! unverified challenge's claim is never recorded as fact), the presented
//! command line, the grant rkey, and the outcome. It never carries the nonce,
//! the challenge signature or any key other than the doorkeeper's own.
//! Publication to the player's repository and the #205 review screen are
//! deferred; this is the receipt record and its verification.

use crate::{
    card::digest::epoch_seconds,
    error::{AtmosphericError, Result},
    signing::{self, SigningKey},
    social::triggers::format_utc,
};
use serde::{Deserialize, Serialize};

use super::{
    authorize::{Decision, DenyReason},
    command::MAX_COMMAND_LEN,
    grant::{AgentGrantRecord, GrantKind, MAX_DID_LEN, MAX_RKEY_LEN},
};

/// NSID in the signed payload, preventing replay as a grant.
pub const RECEIPT_COLLECTION: &str = "club.atmospheric.agent.receipt";
const MAX_REASON_LEN: usize = 64;
/// Stands in for every character of a presented line that a receipt cannot
/// carry verbatim (anything outside printable ASCII).
const REPLACEMENT: char = '\u{FFFD}';

/// One doorkeeper decision. `signature` covers every other field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReceipt {
    pub collection: String,
    /// Doorkeeper-local sequence number, strictly increasing per key.
    pub seq: u64,
    pub issued_at: String,
    /// Player whose capabilities were requested: the doorkeeper's owner.
    pub owner: String,
    /// Agent named by the verified grant. Absent when the grant itself did
    /// not verify, because only an unauthenticated claim was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The presented command line, printable ASCII with anything else
    /// replaced by U+FFFD and cut to the command length cap.
    pub command: String,
    /// rkey of the grant the agent presented, when it was well-formed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_rkey: Option<String>,
    /// `allow` or `deny`.
    pub outcome: String,
    /// Stable deny reason, or `allowed`.
    pub reason: String,
    /// SPKI base64 of the Ed25519 key that signed the receipt.
    pub signing_key: String,
    /// Base64 (no padding) Ed25519 signature over every other field.
    pub signature: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    collection: &'a str,
    seq: u64,
    issued_at: &'a str,
    owner: &'a str,
    agent: Option<&'a str>,
    command: &'a str,
    grant_rkey: Option<&'a str>,
    outcome: &'a str,
    reason: &'a str,
    signing_key: &'a str,
}

impl AgentReceipt {
    /// Build an unsigned receipt for `decision`, made at `now` (Unix seconds)
    /// by the doorkeeper guarding `owner`. Every decision — including one on
    /// garbage input — yields a structurally valid receipt.
    pub fn for_decision(
        seq: u64,
        owner: &str,
        grant: &AgentGrantRecord,
        grant_rkey: &str,
        command: &str,
        decision: Decision,
        now: i64,
    ) -> Self {
        let grant_verified = !matches!(
            decision,
            Decision::Deny(
                DenyReason::BadGrant
                    | DenyReason::ReplayedNonce
                    | DenyReason::UnknownCommand
                    | DenyReason::DeniedCommand
                    | DenyReason::CommandInText
                    | DenyReason::ConfirmRequired
                    | DenyReason::MalformedCommand
            )
        ) && grant.kind == GrantKind::Grant
            && grant.owner == owner;
        Self {
            collection: RECEIPT_COLLECTION.to_owned(),
            seq,
            issued_at: format_utc(now),
            owner: owner.to_owned(),
            agent: grant
                .agent
                .clone()
                .filter(|agent| grant_verified && is_did(agent)),
            command: sanitize_command(command),
            grant_rkey: is_rkey(grant_rkey).then(|| grant_rkey.to_owned()),
            outcome: decision.outcome().to_owned(),
            reason: decision.reason().to_owned(),
            signing_key: String::new(),
            signature: String::new(),
        }
    }

    /// Exact bytes the signature covers: every field except `signature`.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let payload = Payload {
            collection: &self.collection,
            seq: self.seq,
            issued_at: &self.issued_at,
            owner: &self.owner,
            agent: self.agent.as_deref(),
            command: &self.command,
            grant_rkey: self.grant_rkey.as_deref(),
            outcome: &self.outcome,
            reason: &self.reason,
            signing_key: &self.signing_key,
        };
        serde_json::to_vec(&payload).expect("the receipt payload is always serialisable")
    }

    /// Fill in the public key and sign. Call after every other field is set.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signing_key = signing::public_key_spki_b64(&key.verifying_key());
        self.signature = signing::sign_b64(key, &self.canonical_bytes());
    }

    /// Structural checks that do not need the signing key.
    pub fn validate_structure(&self) -> std::result::Result<(), String> {
        if self.collection != RECEIPT_COLLECTION {
            return Err(format!("receipt collection must be {RECEIPT_COLLECTION}"));
        }
        if epoch_seconds(&self.issued_at).is_none() {
            return Err("receipt issuedAt must be RFC 3339 UTC".into());
        }
        if !is_did(&self.owner) {
            return Err("receipt owner must be a did:plc or did:web identifier".into());
        }
        if self.agent.as_deref().is_some_and(|agent| !is_did(agent)) {
            return Err("receipt agent must be a did:plc or did:web identifier".into());
        }
        if self.command.chars().count() > MAX_COMMAND_LEN
            || self.command.chars().any(char::is_control)
        {
            return Err("receipt command is malformed".into());
        }
        if self
            .grant_rkey
            .as_deref()
            .is_some_and(|rkey| !is_rkey(rkey))
        {
            return Err("receipt grant rkey is malformed".into());
        }
        let known_reason = self.reason == "allowed"
            || DenyReason::ALL
                .iter()
                .any(|reason| reason.as_str() == self.reason);
        match self.outcome.as_str() {
            "allow" if self.reason == "allowed" => {}
            "deny" if known_reason && self.reason != "allowed" => {}
            "allow" | "deny" => return Err("receipt reason does not match its outcome".into()),
            _ => return Err("receipt outcome must be allow or deny".into()),
        }
        if self.reason.len() > MAX_REASON_LEN {
            return Err("receipt reason is malformed".into());
        }
        Ok(())
    }

    /// Verify the signature with the key the receipt carries. The caller must
    /// still trust that key as the doorkeeper's key.
    pub fn verify(&self) -> Result<()> {
        self.validate_structure()
            .map_err(AtmosphericError::Message)?;
        let key = signing::verifying_key_from_spki_b64(&self.signing_key)?;
        signing::verify_b64(&key, &self.canonical_bytes(), &self.signature)
    }
}

/// Reduce a presented line to something a receipt can carry verbatim.
fn sanitize_command(command: &str) -> String {
    command
        .chars()
        .take(MAX_COMMAND_LEN)
        .map(|ch| {
            if (' '..='~').contains(&ch) {
                ch
            } else {
                REPLACEMENT
            }
        })
        .collect()
}

fn is_did(value: &str) -> bool {
    (value.starts_with("did:plc:") || value.starts_with("did:web:"))
        && value.len() <= MAX_DID_LEN
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
}

fn is_rkey(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RKEY_LEN
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::generate;

    const NOW: i64 = 1_795_000_000;
    const OWNER: &str = "did:plc:player";

    fn grant() -> AgentGrantRecord {
        let mut grant = AgentGrantRecord::new(
            OWNER.into(),
            "did:plc:agent".into(),
            "2026-11-25T00:00:00.000Z".into(),
        );
        grant.commands = vec!["state".into()];
        grant
    }

    #[test]
    fn allowed_receipt_carries_both_dids_and_the_exact_line() {
        let doorkeeper = generate();
        let mut receipt = AgentReceipt::for_decision(
            7,
            OWNER,
            &grant(),
            "3labcdef",
            "state",
            Decision::Allow,
            NOW,
        );
        receipt.sign(&doorkeeper);
        receipt.verify().unwrap();
        assert_eq!(receipt.collection, RECEIPT_COLLECTION);
        assert_eq!(receipt.seq, 7);
        assert_eq!(epoch_seconds(&receipt.issued_at), Some(NOW));
        assert_eq!(receipt.owner, OWNER);
        assert_eq!(receipt.agent.as_deref(), Some("did:plc:agent"));
        assert_eq!(receipt.command, "state");
        assert_eq!(receipt.grant_rkey.as_deref(), Some("3labcdef"));
        assert_eq!(receipt.outcome, "allow");
        assert_eq!(receipt.reason, "allowed");

        let json = serde_json::to_value(&receipt).unwrap();
        let fields: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for field in [
            "collection",
            "seq",
            "issuedAt",
            "owner",
            "agent",
            "command",
            "grantRkey",
            "outcome",
            "reason",
            "signingKey",
            "signature",
        ] {
            assert!(fields.contains(&field), "{field} missing");
        }
        assert_eq!(fields.len(), 11, "no nonce or other secret is recorded");
        let back: AgentReceipt = serde_json::from_value(json).unwrap();
        back.verify().unwrap();
    }

    #[test]
    fn every_field_is_signed() {
        let doorkeeper = generate();
        let mut receipt = AgentReceipt::for_decision(
            1,
            OWNER,
            &grant(),
            "3labcdef",
            "state",
            Decision::Allow,
            NOW,
        );
        receipt.sign(&doorkeeper);
        for (field, mutate) in [
            ("seq", (|r: &mut AgentReceipt| r.seq = 2) as fn(&mut _)),
            ("outcome", |r| {
                r.outcome = "deny".into();
                r.reason = "revoked".into();
            }),
            ("agent", |r| r.agent = None),
            ("command", |r| r.command = "inventory".into()),
            ("grantRkey", |r| r.grant_rkey = Some("3lother".into())),
            ("issuedAt", |r| r.issued_at = format_utc(NOW + 1)),
        ] {
            let mut tampered = receipt.clone();
            mutate(&mut tampered);
            assert!(tampered.verify().is_err(), "{field} is not signed");
        }
    }

    #[test]
    fn every_deny_reason_yields_a_valid_receipt_even_on_garbage_input() {
        let doorkeeper = generate();
        let garbage_grant =
            AgentGrantRecord::new("not a did".into(), "agent\nspoof".into(), String::new());
        let long = "a".repeat(MAX_COMMAND_LEN * 2);
        for reason in DenyReason::ALL {
            for (grant, rkey, command) in [
                (grant(), "3labcdef", "state"),
                (garbage_grant.clone(), "bad rkey\n", "chat hi\n/op me"),
                (grant(), "", long.as_str()),
                (grant(), "3labcdef", "chat \u{1b}[31mhé"),
                (grant(), "3labcdef", ""),
            ] {
                let mut receipt = AgentReceipt::for_decision(
                    0,
                    OWNER,
                    &grant,
                    rkey,
                    command,
                    Decision::Deny(reason),
                    NOW,
                );
                receipt.sign(&doorkeeper);
                receipt
                    .verify()
                    .unwrap_or_else(|error| panic!("{reason:?} {command:?}: {error}"));
                assert_eq!(receipt.outcome, "deny");
                assert_eq!(receipt.reason, reason.as_str());
                assert!(receipt.command.chars().count() <= MAX_COMMAND_LEN);
            }
        }
    }

    #[test]
    fn unverified_agents_and_bad_rkeys_are_not_recorded() {
        let bad_grant = AgentReceipt::for_decision(
            0,
            OWNER,
            &grant(),
            "3labcdef",
            "state",
            Decision::Deny(DenyReason::BadGrant),
            NOW,
        );
        assert_eq!(bad_grant.agent, None);
        let mut foreign = grant();
        foreign.owner = "did:plc:someone".into();
        let foreign = AgentReceipt::for_decision(
            0,
            OWNER,
            &foreign,
            "3labcdef",
            "state",
            Decision::Deny(DenyReason::AgentMismatch),
            NOW,
        );
        assert_eq!(foreign.agent, None);
        let revoked = AgentReceipt::for_decision(
            0,
            OWNER,
            &grant(),
            "bad rkey",
            "chat a\u{7}b",
            Decision::Deny(DenyReason::Revoked),
            NOW,
        );
        assert_eq!(revoked.agent.as_deref(), Some("did:plc:agent"));
        assert_eq!(revoked.grant_rkey, None);
        assert_eq!(revoked.command, "chat a\u{FFFD}b");
    }

    #[test]
    fn structure_is_checked() {
        let receipt = AgentReceipt::for_decision(
            0,
            OWNER,
            &grant(),
            "3labcdef",
            "state",
            Decision::Deny(DenyReason::Revoked),
            NOW,
        );
        assert!(receipt.validate_structure().is_ok());
        let mut bad = receipt.clone();
        bad.owner = "player".into();
        assert!(bad.validate_structure().is_err());
        let mut bad = receipt.clone();
        bad.reason = "expired".into();
        assert!(bad.validate_structure().is_err());
        let mut bad = receipt.clone();
        bad.outcome = "allow".into();
        assert!(bad.validate_structure().is_err());
        let mut bad = receipt.clone();
        bad.reason = "allowed".into();
        assert!(bad.validate_structure().is_err());
        let mut bad = receipt;
        bad.outcome = "maybe".into();
        assert!(bad.validate_structure().is_err());
    }
}
