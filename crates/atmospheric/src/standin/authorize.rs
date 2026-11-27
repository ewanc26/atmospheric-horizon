//! Deny-by-default authorization for Stand-In (#206).
//!
//! This is the local-deny path the owner called orthogonal to native egress:
//! given a signed grant, signed revocations and a signed agent challenge, it
//! either allows one exact command line or returns a stable deny reason. It
//! never touches a socket, runs a process, writes a file or resolves a DID.
//! The doorkeeper resolves the trusted keys and the grant rkey from the
//! player's repository, then calls [`authorize_presented`] before forwarding
//! anything.
//!
//! Checks run cheapest and most local first: the command line is classified
//! against the pinned catalog before any signature is examined, so an
//! unknown or dangerous command is denied for that reason even when the rest
//! of the presentation is also bad.

use crate::{
    error::Result,
    signing::{verify_b64, verifying_key_from_spki_b64},
};
use serde::{Deserialize, Serialize};

use super::{
    challenge::AgentChallenge,
    command::{self, CommandClass},
    grant::{AgentGrantRecord, GrantKind},
    nonce::NonceBook,
};

/// Stable deny reasons, safe to put in a signed audit receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The grant failed structure or signature checks with the trusted key.
    BadGrant,
    /// A valid signed revocation names the grant rkey.
    Revoked,
    /// The wall clock is outside the grant's window.
    GrantInactive,
    /// The challenge failed structure or signature checks.
    BadChallenge,
    /// The challenge is outside its short replay window.
    StaleChallenge,
    /// The presenting agent is not the grant's agent.
    AgentMismatch,
    /// The challenge does not carry the nonce this doorkeeper issued.
    NonceMismatch,
    /// The challenge's nonce was never issued, already used or expired.
    ReplayedNonce,
    /// The challenge was signed for a different command line.
    CommandMismatch,
    /// The command name is unknown in the pinned catalog.
    UnknownCommand,
    /// The name is known but categorically denied to agents.
    DeniedCommand,
    /// `chat`/`type` would run a server command instead of plain text.
    CommandInText,
    /// The line carries the upstream destructive confirmation bypass.
    ConfirmRequired,
    /// The line is empty, oversized or contains control characters.
    MalformedCommand,
    /// The command is eligible but not exactly granted.
    CommandNotGranted,
}

impl DenyReason {
    /// Every deny reason, in declaration order.
    pub const ALL: [Self; 15] = [
        Self::BadGrant,
        Self::Revoked,
        Self::GrantInactive,
        Self::BadChallenge,
        Self::StaleChallenge,
        Self::AgentMismatch,
        Self::NonceMismatch,
        Self::ReplayedNonce,
        Self::CommandMismatch,
        Self::UnknownCommand,
        Self::DeniedCommand,
        Self::CommandInText,
        Self::ConfirmRequired,
        Self::MalformedCommand,
        Self::CommandNotGranted,
    ];

    /// Stable wire name, used in receipts and audit logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadGrant => "bad_grant",
            Self::Revoked => "revoked",
            Self::GrantInactive => "grant_inactive",
            Self::BadChallenge => "bad_challenge",
            Self::StaleChallenge => "stale_challenge",
            Self::AgentMismatch => "agent_mismatch",
            Self::NonceMismatch => "nonce_mismatch",
            Self::ReplayedNonce => "replayed_nonce",
            Self::CommandMismatch => "command_mismatch",
            Self::UnknownCommand => CommandClass::Unknown.as_str(),
            Self::DeniedCommand => CommandClass::Denied.as_str(),
            Self::CommandInText => CommandClass::CommandInText.as_str(),
            Self::ConfirmRequired => CommandClass::Confirm.as_str(),
            Self::MalformedCommand => CommandClass::Malformed.as_str(),
            Self::CommandNotGranted => "command_not_granted",
        }
    }
}

/// Result of one doorkeeper decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
}

impl Decision {
    /// Stable wire name for the receipt's `outcome` field.
    pub fn outcome(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny(_) => "deny",
        }
    }

    /// Stable wire name for the receipt's `reason` field.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Allow => "allowed",
            Self::Deny(reason) => reason.as_str(),
        }
    }
}

impl From<CommandClass> for DenyReason {
    fn from(class: CommandClass) -> Self {
        match class {
            CommandClass::Delegable => Self::CommandNotGranted,
            CommandClass::Unknown => Self::UnknownCommand,
            CommandClass::Denied => Self::DeniedCommand,
            CommandClass::CommandInText => Self::CommandInText,
            CommandClass::Confirm => Self::ConfirmRequired,
            CommandClass::Malformed => Self::MalformedCommand,
        }
    }
}

fn verify_with_trusted_key(canonical: Vec<u8>, signature: &str, key_b64: &str) -> Result<()> {
    let key = verifying_key_from_spki_b64(key_b64)?;
    verify_b64(&key, &canonical, signature)
}

/// Authorize one exact command line at `now_epoch`.
///
/// `owner` is the DID of the player whose control socket this doorkeeper
/// guards; a grant from anyone else is [`DenyReason::BadGrant`], however well
/// it is signed. `owner_key_b64` and `agent_key_b64` are SPKI base64 keys the caller has
/// already resolved and trusted for the two DIDs. `grant_rkey` names the grant
/// record in the owner's repository; signed revocations naming that rkey are
/// checked. `expected_nonce` is the nonce this doorkeeper issued, and
/// `command` is the exact line it would forward to `horizon-ctl`. Prefer
/// [`authorize_presented`], which also burns the nonce.
#[allow(clippy::too_many_arguments)]
pub fn authorize(
    owner: &str,
    grant: &AgentGrantRecord,
    owner_key_b64: &str,
    grant_rkey: &str,
    revocations: &[AgentGrantRecord],
    challenge: &AgentChallenge,
    agent_key_b64: &str,
    expected_nonce: &str,
    command: &str,
    now_epoch: i64,
) -> Decision {
    let class = command::classify(command);
    if class != CommandClass::Delegable {
        return Decision::Deny(class.into());
    }
    if grant.owner != owner
        || grant.kind != GrantKind::Grant
        || grant.validate_structure().is_err()
        || verify_with_trusted_key(grant.canonical_bytes(), &grant.signature, owner_key_b64)
            .is_err()
    {
        return Decision::Deny(DenyReason::BadGrant);
    }
    if revocations.iter().any(|record| {
        record.kind == GrantKind::Revocation
            && record.owner == grant.owner
            && record.target.as_deref() == Some(grant_rkey)
            && record.validate_structure().is_ok()
            && verify_with_trusted_key(record.canonical_bytes(), &record.signature, owner_key_b64)
                .is_ok()
    }) {
        return Decision::Deny(DenyReason::Revoked);
    }
    if !grant.is_active_at(now_epoch) {
        return Decision::Deny(DenyReason::GrantInactive);
    }
    if challenge.validate_structure().is_err()
        || verify_with_trusted_key(
            challenge.canonical_bytes(),
            &challenge.signature,
            agent_key_b64,
        )
        .is_err()
    {
        return Decision::Deny(DenyReason::BadChallenge);
    }
    if !challenge.is_current(now_epoch) {
        return Decision::Deny(DenyReason::StaleChallenge);
    }
    if grant.agent.as_deref() != Some(challenge.agent.as_str()) {
        return Decision::Deny(DenyReason::AgentMismatch);
    }
    if challenge.nonce != expected_nonce {
        return Decision::Deny(DenyReason::NonceMismatch);
    }
    if challenge.command != command {
        return Decision::Deny(DenyReason::CommandMismatch);
    }
    if !grant.grants(command) {
        return Decision::Deny(DenyReason::CommandNotGranted);
    }
    Decision::Allow
}

/// Authorize a presentation against the doorkeeper's [`NonceBook`].
///
/// The challenge's nonce is consumed first, on every presentation, so a
/// challenge can never be redeemed twice even when the first attempt was
/// denied. A nonce the book does not hold, or holds past its TTL, is
/// [`DenyReason::ReplayedNonce`]; otherwise this is [`authorize`] with that
/// nonce as the expected one.
#[allow(clippy::too_many_arguments)]
pub fn authorize_presented(
    nonces: &mut NonceBook,
    owner: &str,
    grant: &AgentGrantRecord,
    owner_key_b64: &str,
    grant_rkey: &str,
    revocations: &[AgentGrantRecord],
    challenge: &AgentChallenge,
    agent_key_b64: &str,
    command: &str,
    now_epoch: i64,
) -> Decision {
    if !nonces.consume(&challenge.nonce, now_epoch) {
        return Decision::Deny(DenyReason::ReplayedNonce);
    }
    authorize(
        owner,
        grant,
        owner_key_b64,
        grant_rkey,
        revocations,
        challenge,
        agent_key_b64,
        &challenge.nonce,
        command,
        now_epoch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        card::digest::epoch_seconds,
        signing::{SigningKey, generate, public_key_spki_b64},
        standin::{challenge::AgentChallenge, grant::AgentGrantRecord},
    };

    const RKEY: &str = "3labcdef";
    const OWNER: &str = "did:plc:player";

    fn now() -> i64 {
        epoch_seconds("2026-11-25T12:00:00.000Z").unwrap()
    }

    struct Fixture {
        owner: SigningKey,
        agent: SigningKey,
        owner_key_b64: String,
        agent_key_b64: String,
        nonces: NonceBook,
        grant: AgentGrantRecord,
    }

    impl Fixture {
        fn new(commands: &[&str]) -> Self {
            let owner = generate();
            let agent = generate();
            let mut grant = AgentGrantRecord::new(
                "did:plc:player".into(),
                "did:plc:agent".into(),
                "2026-11-25T00:00:00.000Z".into(),
            );
            grant.commands = commands.iter().map(|line| (*line).to_owned()).collect();
            grant.expires_at = Some("2026-11-26T00:00:00.000Z".into());
            grant.sign(&owner);
            Self {
                owner_key_b64: public_key_spki_b64(&owner.verifying_key()),
                agent_key_b64: public_key_spki_b64(&agent.verifying_key()),
                owner,
                agent,
                nonces: NonceBook::new(),
                grant,
            }
        }

        /// A correctly signed challenge over a freshly issued nonce.
        fn challenge(&mut self, did: &str, command: &str) -> AgentChallenge {
            let nonce = self.nonces.issue(now()).unwrap();
            AgentChallenge::sign_at(did.into(), nonce, command.into(), now(), 60, &self.agent)
                .unwrap()
        }

        fn present(
            &mut self,
            grant: &AgentGrantRecord,
            revocations: &[AgentGrantRecord],
            challenge: &AgentChallenge,
            command: &str,
            at: i64,
        ) -> Decision {
            authorize_presented(
                &mut self.nonces,
                OWNER,
                grant,
                &self.owner_key_b64,
                RKEY,
                revocations,
                challenge,
                &self.agent_key_b64,
                command,
                at,
            )
        }

        fn run(&mut self, command: &str) -> Decision {
            let challenge = self.challenge("did:plc:agent", command);
            let grant = self.grant.clone();
            self.present(&grant, &[], &challenge, command, now())
        }

        fn revocation(&self, target: &str, key: &SigningKey) -> AgentGrantRecord {
            let mut revocation = AgentGrantRecord::revocation(
                "did:plc:player".into(),
                target.into(),
                "2026-11-25T01:00:00.000Z".into(),
            );
            revocation.sign(key);
            revocation
        }
    }

    #[test]
    fn exact_delegated_command_is_allowed() {
        let mut f = Fixture::new(&["move forward 2", "break 1 2 3"]);
        let decision = f.run("move forward 2");
        assert_eq!(decision, Decision::Allow);
        assert_eq!(decision.outcome(), "allow");
        assert_eq!(decision.reason(), "allowed");
        assert_eq!(f.run("break 1 2 3"), Decision::Allow);
    }

    #[test]
    fn deny_by_default_without_an_exact_grant_line() {
        let mut f = Fixture::new(&["move forward 2"]);
        for command in [
            "state",
            "move forward 3",
            "move forward 2 --sprint",
            "inventory",
        ] {
            let decision = f.run(command);
            assert_eq!(
                decision,
                Decision::Deny(DenyReason::CommandNotGranted),
                "{command}"
            );
            assert_eq!(decision.outcome(), "deny");
            assert_eq!(decision.reason(), "command_not_granted");
        }
    }

    #[test]
    fn unknown_and_dangerous_commands_fail_closed_first() {
        let mut f = Fixture::new(&["state"]);
        let challenge = f.challenge("did:plc:agent", "state");
        let cases = [
            ("not-a-command", DenyReason::UnknownCommand),
            ("give diamond 64", DenyReason::DeniedCommand),
            ("world-delete w --confirm", DenyReason::DeniedCommand),
            ("chat /op me", DenyReason::CommandInText),
            ("break 1 2 3 --confirm", DenyReason::ConfirmRequired),
            ("chat \"/op me\"", DenyReason::MalformedCommand),
        ];
        for (command, reason) in cases {
            // A broken grant and a mismatched challenge do not mask the reason.
            let decision = authorize(
                OWNER,
                &AgentGrantRecord::new(String::new(), String::new(), String::new()),
                &f.owner_key_b64,
                RKEY,
                &[],
                &challenge,
                &f.agent_key_b64,
                &challenge.nonce,
                command,
                now(),
            );
            assert_eq!(decision, Decision::Deny(reason), "{command}");
        }
    }

    #[test]
    fn revoked_grants_are_denied_and_foreign_revocations_ignored() {
        let mut f = Fixture::new(&["state"]);
        let grant = f.grant.clone();
        let revocation = f.revocation(RKEY, &f.owner);
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&grant, &[revocation], &challenge, "state", now()),
            Decision::Deny(DenyReason::Revoked)
        );

        let foreign = f.revocation(RKEY, &generate());
        let other_target = f.revocation("3lzzzzzz", &f.owner);
        let mut other_owner = f.revocation(RKEY, &f.owner);
        other_owner.owner = "did:plc:someone".into();
        other_owner.sign(&f.owner);
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(
                &grant,
                &[foreign, other_target, other_owner],
                &challenge,
                "state",
                now()
            ),
            Decision::Allow
        );
    }

    #[test]
    fn expired_and_future_grants_are_inactive() {
        let mut f = Fixture::new(&["state"]);
        let mut expired = f.grant.clone();
        expired.expires_at = Some("2026-11-25T06:00:00.000Z".into());
        expired.sign(&f.owner);
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&expired, &[], &challenge, "state", now()),
            Decision::Deny(DenyReason::GrantInactive)
        );

        let mut future = f.grant.clone();
        future.not_before = Some("2026-11-25T13:00:00.000Z".into());
        future.sign(&f.owner);
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&future, &[], &challenge, "state", now()),
            Decision::Deny(DenyReason::GrantInactive)
        );
    }

    #[test]
    fn grants_need_the_trusted_owner_key() {
        let mut f = Fixture::new(&["state"]);
        let mut forged = f.grant.clone();
        forged.sign(&generate());
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&forged, &[], &challenge, "state", now()),
            Decision::Deny(DenyReason::BadGrant)
        );
        let mut widened = f.grant.clone();
        widened.commands.push("inventory".into());
        let challenge = f.challenge("did:plc:agent", "inventory");
        assert_eq!(
            f.present(&widened, &[], &challenge, "inventory", now()),
            Decision::Deny(DenyReason::BadGrant)
        );
        let revocation = f.revocation(RKEY, &f.owner);
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&revocation, &[], &challenge, "state", now()),
            Decision::Deny(DenyReason::BadGrant)
        );
    }

    #[test]
    fn grants_from_another_player_are_rejected_even_when_validly_signed() {
        let mut f = Fixture::new(&["state"]);
        let mallory = generate();
        let mut foreign = f.grant.clone();
        foreign.owner = "did:plc:mallory".into();
        foreign.sign(&mallory);
        foreign.verify().unwrap();
        // The caller mistakenly trusts the key of the grant's own owner.
        f.owner_key_b64 = public_key_spki_b64(&mallory.verifying_key());
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&foreign, &[], &challenge, "state", now()),
            Decision::Deny(DenyReason::BadGrant)
        );
    }

    #[test]
    fn challenges_are_verified_bound_and_short_lived() {
        let mut f = Fixture::new(&["state", "inventory"]);
        let grant = f.grant.clone();

        let mut forged = f.challenge("did:plc:agent", "state");
        forged.sign(&generate());
        assert_eq!(
            f.present(&grant, &[], &forged, "state", now()),
            Decision::Deny(DenyReason::BadChallenge)
        );

        let mut tampered = f.challenge("did:plc:agent", "state");
        tampered.command = "inventory".into();
        assert_eq!(
            f.present(&grant, &[], &tampered, "inventory", now()),
            Decision::Deny(DenyReason::BadChallenge)
        );

        let stale = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&grant, &[], &stale, "state", now() + 61),
            Decision::Deny(DenyReason::StaleChallenge)
        );

        let other_agent = f.challenge("did:plc:someone-else", "state");
        assert_eq!(
            f.present(&grant, &[], &other_agent, "state", now()),
            Decision::Deny(DenyReason::AgentMismatch)
        );

        let mismatched = f.challenge("did:plc:agent", "inventory");
        assert_eq!(
            f.present(&grant, &[], &mismatched, "state", now()),
            Decision::Deny(DenyReason::CommandMismatch)
        );
    }

    #[test]
    fn nonces_are_single_use_and_issued_by_the_doorkeeper() {
        let mut f = Fixture::new(&["state"]);
        let grant = f.grant.clone();
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            f.present(&grant, &[], &challenge, "state", now()),
            Decision::Allow
        );
        assert_eq!(
            f.present(&grant, &[], &challenge, "state", now()),
            Decision::Deny(DenyReason::ReplayedNonce)
        );

        // A denied presentation burns its nonce too.
        let denied = f.challenge("did:plc:agent", "inventory");
        assert_eq!(
            f.present(&grant, &[], &denied, "inventory", now()),
            Decision::Deny(DenyReason::CommandNotGranted)
        );
        assert_eq!(
            f.present(&grant, &[], &denied, "inventory", now()),
            Decision::Deny(DenyReason::ReplayedNonce)
        );

        // A nonce the agent made up was never issued.
        let invented = AgentChallenge::sign_at(
            "did:plc:agent".into(),
            "0123456789abcdef0123456789abcdef".into(),
            "state".into(),
            now(),
            60,
            &f.agent,
        )
        .unwrap();
        assert_eq!(
            f.present(&grant, &[], &invented, "state", now()),
            Decision::Deny(DenyReason::ReplayedNonce)
        );

        // The low-level check still compares against the expected nonce.
        let challenge = f.challenge("did:plc:agent", "state");
        assert_eq!(
            authorize(
                OWNER,
                &grant,
                &f.owner_key_b64,
                RKEY,
                &[],
                &challenge,
                &f.agent_key_b64,
                "wrong-nonce",
                "state",
                now(),
            ),
            Decision::Deny(DenyReason::NonceMismatch)
        );
    }

    #[test]
    fn deny_reasons_have_stable_wire_names() {
        let names = [
            (DenyReason::BadGrant, "bad_grant"),
            (DenyReason::Revoked, "revoked"),
            (DenyReason::GrantInactive, "grant_inactive"),
            (DenyReason::BadChallenge, "bad_challenge"),
            (DenyReason::StaleChallenge, "stale_challenge"),
            (DenyReason::AgentMismatch, "agent_mismatch"),
            (DenyReason::NonceMismatch, "nonce_mismatch"),
            (DenyReason::ReplayedNonce, "replayed_nonce"),
            (DenyReason::CommandMismatch, "command_mismatch"),
            (DenyReason::UnknownCommand, "unknown_command"),
            (DenyReason::DeniedCommand, "denied_command"),
            (DenyReason::CommandInText, "command_in_text"),
            (DenyReason::ConfirmRequired, "confirm_required"),
            (DenyReason::MalformedCommand, "malformed_command"),
            (DenyReason::CommandNotGranted, "command_not_granted"),
        ];
        assert_eq!(names.map(|(reason, _)| reason), DenyReason::ALL);
        for (reason, name) in names {
            assert_eq!(reason.as_str(), name);
            assert_eq!(serde_json::to_value(reason).unwrap(), name);
        }
    }
}
