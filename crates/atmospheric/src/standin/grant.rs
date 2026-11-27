//! Signed player → agent delegation records (#206).
//!
//! A grant is narrow by construction: an exact, pinned command line list, a
//! required time window of at most [`MAX_GRANT_SECS`], and one named agent
//! DID; there is no blanket "act as me" form. A revocation is a second
//! signed record naming the grant's rkey. Records carry their collection NSID
//! in the signed payload, so a signature cannot be replayed under another
//! lexicon. This module verifies structure and signatures; trusting the
//! owner's key and fetching revocations from its repository are the caller's
//! job.

use crate::{
    card::digest::epoch_seconds,
    error::{AtmosphericError, Result},
    signing::{self, SigningKey},
};
use serde::{Deserialize, Serialize};

/// Collection both grants and revocations are published under.
pub const GRANT_COLLECTION: &str = "club.atmospheric.agent.grant";
/// Maximum length of a DID, rkey or revocation pointer.
pub const MAX_DID_LEN: usize = 256;
pub const MAX_RKEY_LEN: usize = 64;
pub const MAX_POINTER_LEN: usize = 512;
/// A grant carries at most this many exact command lines.
pub const MAX_COMMANDS: usize = 64;
/// Longest window a grant may cover, measured from `notBefore` (or
/// `createdAt` when absent) to `expiresAt`: grants are short-lived.
pub const MAX_GRANT_SECS: i64 = 24 * 60 * 60;

/// What the record asserts. A revocation ends the grant named by `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantKind {
    Grant,
    Revocation,
}

impl GrantKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revocation => "revocation",
        }
    }
}

impl std::str::FromStr for GrantKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim() {
            "grant" => Ok(Self::Grant),
            "revocation" => Ok(Self::Revocation),
            other => Err(format!("unknown grant kind: {other:?}")),
        }
    }
}

/// A signed grant or revocation as published in a player's repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGrantRecord {
    /// Signed to stop replay under another collection.
    pub collection: String,
    pub kind: GrantKind,
    /// DID of the player delegating their own capabilities.
    pub owner: String,
    /// DID of the agent receiving them. Absent on revocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Exact `horizon-ctl` command lines. Absent on revocations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// First instant the grant applies, RFC 3339 UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<String>,
    /// Instant the grant stops applying, RFC 3339 UTC. Required on grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// At-URI of the record that revokes this grant, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation: Option<String>,
    /// rkey of the grant a revocation ends. Absent on grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// RFC 3339 UTC creation time.
    pub created_at: String,
    /// SPKI base64 of the Ed25519 key that signed the record.
    pub signing_key: String,
    /// Base64 (no padding) Ed25519 signature over every other field.
    pub signature: String,
}

/// The signed subset, in a fixed field order shared by signer and verifier.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload<'a> {
    collection: &'a str,
    kind: &'a str,
    owner: &'a str,
    agent: Option<&'a str>,
    commands: &'a [String],
    not_before: Option<&'a str>,
    expires_at: Option<&'a str>,
    revocation: Option<&'a str>,
    target: Option<&'a str>,
    created_at: &'a str,
    signing_key: &'a str,
}

impl AgentGrantRecord {
    /// A minimal signed grant with no commands yet. Callers fill the fields
    /// before calling [`Self::sign`].
    pub fn new(owner: String, agent: String, created_at: String) -> Self {
        Self {
            collection: GRANT_COLLECTION.to_owned(),
            kind: GrantKind::Grant,
            owner,
            agent: Some(agent),
            commands: Vec::new(),
            not_before: None,
            expires_at: None,
            revocation: None,
            target: None,
            created_at,
            signing_key: String::new(),
            signature: String::new(),
        }
    }

    /// A signed revocation naming a grant rkey.
    pub fn revocation(owner: String, target: String, created_at: String) -> Self {
        Self {
            collection: GRANT_COLLECTION.to_owned(),
            kind: GrantKind::Revocation,
            owner,
            agent: None,
            commands: Vec::new(),
            not_before: None,
            expires_at: None,
            revocation: None,
            target: Some(target),
            created_at,
            signing_key: String::new(),
            signature: String::new(),
        }
    }

    /// Exact bytes the signature covers: every field except `signature`.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let payload = Payload {
            collection: &self.collection,
            kind: self.kind.as_str(),
            owner: &self.owner,
            agent: self.agent.as_deref(),
            commands: &self.commands,
            not_before: self.not_before.as_deref(),
            expires_at: self.expires_at.as_deref(),
            revocation: self.revocation.as_deref(),
            target: self.target.as_deref(),
            created_at: &self.created_at,
            signing_key: &self.signing_key,
        };
        serde_json::to_vec(&payload).expect("the grant payload is always serialisable")
    }

    /// Fill in the public key and sign. Call after every other field is set.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signing_key = signing::public_key_spki_b64(&key.verifying_key());
        self.signature = signing::sign_b64(key, &self.canonical_bytes());
    }

    /// Structural checks that do not need the signing key.
    pub fn validate_structure(&self) -> std::result::Result<(), String> {
        validate_grant_spec(
            &self.collection,
            self.kind,
            &self.owner,
            self.agent.as_deref(),
            &self.commands,
            self.not_before.as_deref(),
            self.expires_at.as_deref(),
            self.revocation.as_deref(),
            self.target.as_deref(),
            &self.created_at,
        )
    }

    /// Verify the signature over the canonical bytes with the key the record
    /// carries. The caller must still trust that key for `owner`.
    pub fn verify(&self) -> Result<()> {
        self.validate_structure()
            .map_err(AtmosphericError::Message)?;
        let key = signing::verifying_key_from_spki_b64(&self.signing_key)?;
        signing::verify_b64(&key, &self.canonical_bytes(), &self.signature)
    }

    /// Whether the grant applies at `now_epoch`. Revocations are records, not
    /// timed grants, so they are never active themselves. A missing or
    /// unparseable timestamp is inactive, never open-ended.
    pub fn is_active_at(&self, now_epoch: i64) -> bool {
        if self.kind != GrantKind::Grant {
            return false;
        }
        let Some(created) = epoch_seconds(&self.created_at) else {
            return false;
        };
        let start = match self.not_before.as_deref() {
            Some(start) => match epoch_seconds(start) {
                Some(start) => start,
                None => return false,
            },
            None => created,
        };
        let Some(expires) = self.expires_at.as_deref().and_then(epoch_seconds) else {
            return false;
        };
        created <= now_epoch && start <= now_epoch && now_epoch < expires
    }

    /// Whether `command` is exactly one of the granted lines. Eligibility of
    /// the line itself is [`super::command::classify`]'s job.
    pub fn grants(&self, command: &str) -> bool {
        self.kind == GrantKind::Grant && self.commands.iter().any(|line| line == command)
    }
}

/// Structural validation shared by record verification and input handling.
#[allow(clippy::too_many_arguments)]
pub fn validate_grant_spec(
    collection: &str,
    kind: GrantKind,
    owner: &str,
    agent: Option<&str>,
    commands: &[String],
    not_before: Option<&str>,
    expires_at: Option<&str>,
    revocation: Option<&str>,
    target: Option<&str>,
    created_at: &str,
) -> std::result::Result<(), String> {
    if collection != GRANT_COLLECTION {
        return Err(format!("grant collection must be {GRANT_COLLECTION}"));
    }
    validate_did(owner, "owner")?;
    if epoch_seconds(created_at).is_none() {
        return Err("grant createdAt must be RFC 3339 UTC".into());
    }
    match kind {
        GrantKind::Grant => {
            let agent = agent.ok_or_else(|| "a grant needs an agent DID".to_owned())?;
            validate_did(agent, "agent")?;
            if commands.is_empty() || commands.len() > MAX_COMMANDS {
                return Err(format!(
                    "a grant needs 1..{MAX_COMMANDS} exact command lines"
                ));
            }
            for line in commands {
                if super::command::classify(line) != super::command::CommandClass::Delegable {
                    return Err(format!("grant command {line:?} is not delegable"));
                }
            }
            for (index, line) in commands.iter().enumerate() {
                if commands[..index].contains(line) {
                    return Err(format!("grant command {line:?} is listed twice"));
                }
            }
            if agent == owner {
                return Err("an agent cannot grant itself capabilities".into());
            }
            let created = epoch_seconds(created_at).unwrap_or_default();
            let start = match not_before {
                Some(start) => {
                    let start = epoch_seconds(start)
                        .ok_or_else(|| "grant notBefore must be RFC 3339 UTC".to_owned())?;
                    if start < created {
                        return Err("grant notBefore cannot precede createdAt".into());
                    }
                    start
                }
                None => created,
            };
            let end = expires_at
                .ok_or_else(|| "a grant needs an expiresAt".to_owned())
                .and_then(|end| {
                    epoch_seconds(end)
                        .ok_or_else(|| "grant expiresAt must be RFC 3339 UTC".to_owned())
                })?;
            if end <= start {
                return Err("grant expiresAt must be after its start".into());
            }
            if end - start > MAX_GRANT_SECS {
                return Err("a grant window cannot exceed 24 hours".into());
            }
            if target.is_some() {
                return Err("a grant cannot target another grant".into());
            }
        }
        GrantKind::Revocation => {
            if agent.is_some() || !commands.is_empty() {
                return Err("a revocation cannot carry agent capabilities".into());
            }
            if not_before.is_some() || expires_at.is_some() {
                return Err("a revocation cannot carry a grant window".into());
            }
            let target = target.ok_or_else(|| "a revocation needs a grant rkey".to_owned())?;
            if target.is_empty()
                || target.len() > MAX_RKEY_LEN
                || target.chars().any(char::is_whitespace)
                || target.chars().any(char::is_control)
            {
                return Err("revocation target is malformed".into());
            }
        }
    }
    if let Some(pointer) = revocation
        && (!pointer.starts_with("at://")
            || pointer.len() > MAX_POINTER_LEN
            || pointer.chars().any(char::is_control))
    {
        return Err("grant revocation pointer is malformed".into());
    }
    Ok(())
}

fn validate_did(value: &str, label: &str) -> std::result::Result<(), String> {
    if !(value.starts_with("did:plc:") || value.starts_with("did:web:"))
        || value.len() > MAX_DID_LEN
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(format!("{label} must be a did:plc or did:web identifier"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{self, SigningKey};

    fn key() -> SigningKey {
        signing::generate()
    }

    fn at(iso: &str) -> i64 {
        epoch_seconds(iso).unwrap()
    }

    fn grant(commands: &[&str]) -> AgentGrantRecord {
        let mut grant = AgentGrantRecord::new(
            "did:plc:player".into(),
            "did:plc:agent".into(),
            "2026-11-25T00:00:00.000Z".into(),
        );
        grant.commands = commands.iter().map(|line| (*line).to_owned()).collect();
        grant.expires_at = Some("2026-11-25T00:15:00.000Z".into());
        grant
    }

    #[test]
    fn signed_grant_verifies_and_binds_every_field() {
        let player = key();
        let mut grant = grant(&["move forward 2", "break 1 2 3"]);
        grant.sign(&player);
        grant.verify().unwrap();
        assert!(grant.is_active_at(at("2026-11-25T00:01:00.000Z")));
        let mut tampered = grant.clone();
        tampered.commands.push("chat hello".into());
        assert!(tampered.verify().is_err());
        let mut tampered = grant.clone();
        tampered.agent = Some("did:plc:other".into());
        assert!(tampered.verify().is_err());
        let mut tampered = grant.clone();
        tampered.expires_at = Some("2026-11-25T01:00:00.000Z".into());
        assert!(tampered.verify().is_err());
        let mut tampered = grant;
        tampered.collection = "club.atmospheric.warrant".into();
        assert!(tampered.verify().is_err());
    }

    #[test]
    fn json_round_trips_with_camel_case_fields() {
        let mut grant = grant(&["state"]);
        grant.sign(&key());
        let json = serde_json::to_value(&grant).unwrap();
        assert_eq!(json["collection"], GRANT_COLLECTION);
        assert_eq!(json["kind"], "grant");
        assert_eq!(json["expiresAt"], "2026-11-25T00:15:00.000Z");
        assert!(json.get("target").is_none());
        let back: AgentGrantRecord = serde_json::from_value(json).unwrap();
        back.verify().unwrap();
        assert_eq!(back, grant);
    }

    #[test]
    fn revocation_is_signed_and_bounded() {
        let player = key();
        let mut revocation = AgentGrantRecord::revocation(
            "did:plc:player".into(),
            "3labcdef".into(),
            "2026-11-25T00:00:00.000Z".into(),
        );
        revocation.sign(&player);
        revocation.verify().unwrap();
        assert!(!revocation.is_active_at(i64::MAX));
        assert!(!revocation.grants("state"));
        let mut bad = revocation.clone();
        bad.target = Some("".into());
        assert!(bad.verify().is_err());
        let mut bad = revocation;
        bad.commands = vec!["state".into()];
        assert!(bad.validate_structure().is_err());
    }

    #[test]
    fn dangerous_unknown_and_duplicate_commands_cannot_be_granted() {
        assert!(
            grant(&["setblock 1 2 3 stone"])
                .validate_structure()
                .is_err()
        );
        assert!(grant(&["not-a-command"]).validate_structure().is_err());
        assert!(grant(&["chat /op me"]).validate_structure().is_err());
        assert!(grant(&["state", "state"]).validate_structure().is_err());
        assert!(grant(&[]).validate_structure().is_err());
        let many: Vec<String> = (0..=MAX_COMMANDS).map(|n| format!("select {n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(grant(&many).validate_structure().is_err());
    }

    #[test]
    fn identities_are_validated() {
        let mut self_grant = grant(&["state"]);
        self_grant.agent = Some("did:plc:player".into());
        assert!(self_grant.validate_structure().is_err());
        let mut handle = grant(&["state"]);
        handle.agent = Some("agent.bsky.social".into());
        assert!(handle.validate_structure().is_err());
        let mut no_agent = grant(&["state"]);
        no_agent.agent = None;
        assert!(no_agent.validate_structure().is_err());
    }

    #[test]
    fn windows_are_required_short_and_ordered() {
        let mut inverted = grant(&["state"]);
        inverted.not_before = Some("2026-11-25T00:20:00.000Z".into());
        assert!(inverted.validate_structure().is_err());
        let mut early = grant(&["state"]);
        early.not_before = Some("2026-11-24T00:00:00.000Z".into());
        assert!(early.validate_structure().is_err());
        let mut open_ended = grant(&["state"]);
        open_ended.expires_at = None;
        assert!(open_ended.validate_structure().is_err());
        assert!(!open_ended.is_active_at(at("2026-11-25T00:01:00.000Z")));
        let mut long = grant(&["state"]);
        long.expires_at = Some("2026-11-26T00:00:01.000Z".into());
        assert!(long.validate_structure().is_err());
        let mut day = grant(&["state"]);
        day.expires_at = Some("2026-11-26T00:00:00.000Z".into());
        assert!(day.validate_structure().is_ok());
    }

    #[test]
    fn activity_is_bounded_by_the_window() {
        let mut timed = grant(&["state"]);
        timed.not_before = Some("2026-11-25T00:05:00.000Z".into());
        assert!(!timed.is_active_at(at("2026-11-24T23:59:59.000Z")));
        assert!(!timed.is_active_at(at("2026-11-25T00:04:59.000Z")));
        assert!(timed.is_active_at(at("2026-11-25T00:05:00.000Z")));
        assert!(timed.is_active_at(at("2026-11-25T00:14:59.000Z")));
        assert!(!timed.is_active_at(at("2026-11-25T00:15:00.000Z")));
        let mut garbled = timed.clone();
        garbled.not_before = Some("soon".into());
        assert!(!garbled.is_active_at(at("2026-11-25T00:10:00.000Z")));
    }

    #[test]
    fn grants_match_exact_lines_only() {
        let grant = grant(&["move forward 2", "break 1 2 3"]);
        assert!(grant.grants("move forward 2"));
        assert!(grant.grants("break 1 2 3"));
        assert!(!grant.grants("move forward 20"));
        assert!(!grant.grants("move forward 2 "));
        assert!(!grant.grants("move  forward 2"));
        assert!(!grant.grants("break 1 2 4"));
        assert!(!grant.grants("move"));
    }
}
