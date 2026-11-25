//! Private ballots and the receipts voters keep (#193).
//!
//! A voter's choice is hidden in a salted commitment; only the commitment,
//! the DID and a deterministic nullifier enter the Merkle tree. The server
//! publishes the root, participant list and aggregate tally. Each voter keeps
//! a receipt containing their choice, salt, nullifier, leaf position and
//! sibling hashes, so they can prove inclusion without the choice ever being
//! published next to their identity.
//!
//! The server can still omit a vote, but the published participant list is
//! its claim of completeness and every included voter can check their own
//! receipt against the signed root.

use crate::error::{AtmosphericError, Result};
use crate::poll::record::PollRecord;
use crate::poll::tree;
use crate::signing::SigningKey;
use getrandom::{SysRng, rand_core::Rng, rand_core::UnwrapErr};
use serde::{Deserialize, Serialize};

/// A private ballot before the tree is built. The voter (or the worker on
/// their behalf) keeps `choice` and `salt`; the tree stores only the
/// commitment and nullifier derived from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ballot {
    /// The voter's canonical DID.
    pub did: String,
    /// The zero-based choice index.
    pub choice: u16,
    /// Fresh random salt that hides the choice in the commitment.
    pub salt: [u8; 32],
    /// Salted commitment to the choice.
    pub commitment: [u8; 32],
    /// One-vote-per-DID nullifier for this poll.
    pub nullifier: [u8; 32],
    /// The leaf committed to the Merkle tree.
    pub leaf: [u8; 32],
}

impl Ballot {
    /// Build a ballot from a poll id, a voter DID, a choice and a salt. The
    /// poll id is the record's AT-URI, so nullifiers cannot be replayed
    /// across polls.
    pub fn new(poll_id: &str, did: &str, choice: u16, salt: [u8; 32]) -> Self {
        let commitment = tree::commitment(choice, &salt);
        let nullifier = tree::nullifier(poll_id, did);
        let leaf = tree::leaf(did, &commitment, &nullifier);
        Self {
            did: did.to_string(),
            choice,
            salt,
            commitment,
            nullifier,
            leaf,
        }
    }

    /// Draw a fresh 32-byte salt from the OS randomness source.
    pub fn fresh_salt() -> [u8; 32] {
        let mut salt = [0u8; 32];
        UnwrapErr(SysRng).fill_bytes(&mut salt);
        salt
    }
}

/// The proof a voter keeps. It contains the private data needed to rebuild
/// the leaf plus the sibling hashes needed to prove that leaf is in the
/// signed root. It is intentionally never published with the tally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub did: String,
    pub choice: u16,
    /// Lower-hex salt, so the receipt survives a JSON wire round-trip.
    pub salt: String,
    /// Lower-hex nullifier for this poll.
    pub nullifier: String,
    /// Zero-based leaf position in the submission order.
    pub index: usize,
    /// Lower-hex sibling hashes from the leaf to the root.
    pub siblings: Vec<String>,
}

impl Receipt {
    /// Attach a ballot to its Merkle proof.
    pub fn new(ballot: &Ballot, index: usize, siblings: &[[u8; 32]]) -> Self {
        Self {
            did: ballot.did.clone(),
            choice: ballot.choice,
            salt: tree::hex(&ballot.salt),
            nullifier: tree::hex(&ballot.nullifier),
            index,
            siblings: siblings.iter().map(tree::hex).collect(),
        }
    }

    /// Verify this receipt against a published poll: rebuild the commitment
    /// and leaf, check the nullifier, then walk the proof to the signed root.
    pub fn verify(&self, poll: &PollRecord) -> Result<()> {
        let poll_id = poll_id(&poll.signing_key, &poll.question, &poll.created_at);
        let salt = tree::from_hex(&self.salt)
            .ok_or_else(|| AtmosphericError::Message("receipt salt is bad hex".into()))?;
        let nullifier = tree::from_hex(&self.nullifier)
            .ok_or_else(|| AtmosphericError::Message("receipt nullifier is bad hex".into()))?;
        let root = tree::from_hex(&poll.root)
            .ok_or_else(|| AtmosphericError::Message("poll root is bad hex".into()))?;
        let siblings = self
            .siblings
            .iter()
            .map(|value| {
                tree::from_hex(value)
                    .ok_or_else(|| AtmosphericError::Message("receipt proof has bad hex".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        let expected_nullifier = tree::nullifier(&poll_id, &self.did);
        if nullifier != expected_nullifier {
            return Err(AtmosphericError::Message(
                "receipt nullifier does not match the poll and DID".into(),
            ));
        }
        let commitment = tree::commitment(self.choice, &salt);
        let leaf = tree::leaf(&self.did, &commitment, &nullifier);
        if !tree::verify(&root, &leaf, self.index, &siblings, poll.participants.len()) {
            return Err(AtmosphericError::Message(
                "receipt is not included in the published tally root".into(),
            ));
        }
        Ok(())
    }
}

/// A stable local poll id for nullifier derivation when the record's AT-URI
/// is not in scope (the public verifier only holds the fields themselves).
/// The signing key plus question plus publish time identifies one published
/// tally without depending on the server's chosen record key.
pub fn poll_id(signing_key: &str, question: &str, created_at: &str) -> String {
    format!("poll:{signing_key}:{question}:{created_at}")
}

/// Build a signed tally record from the ordered ballots. The caller supplies
/// the question, choices, close and publish times; this function derives the
/// root, participant list and aggregate tally, signs with `key`, and returns
/// the record together with one receipt per ballot.
pub fn publish(
    question: String,
    options: Vec<String>,
    closes_at: String,
    created_at: String,
    ballots: &[Ballot],
    key: &SigningKey,
) -> Result<(PollRecord, Vec<Receipt>)> {
    if ballots.is_empty() {
        return Err(AtmosphericError::Message(
            "cannot publish a poll with no votes".into(),
        ));
    }
    // The poll id is derived from the public key, question and publish time,
    // so derive it first and build every leaf against the final id. The
    // caller's seed id only matters before signing and is intentionally not
    // used here: a receipt must verify from the published record alone.
    let signing_key = crate::signing::public_key_spki_b64(&key.verifying_key());
    let id = poll_id(&signing_key, &question, &created_at);
    let aligned: Vec<Ballot> = ballots
        .iter()
        .map(|ballot| Ballot::new(&id, &ballot.did, ballot.choice, ballot.salt))
        .collect();
    let leaves: Vec<[u8; 32]> = aligned.iter().map(|ballot| ballot.leaf).collect();
    let root = tree::root(&leaves)
        .ok_or_else(|| AtmosphericError::Message("cannot publish a poll with no votes".into()))?;
    let tally = tally(&options, ballots)?;
    let mut record = PollRecord {
        question,
        options,
        closes_at,
        created_at,
        root: tree::hex(&root),
        participants: aligned.iter().map(|ballot| ballot.did.clone()).collect(),
        tally,
        signing_key,
        signature: String::new(),
    };
    record.sign(key);
    let receipts = aligned
        .iter()
        .enumerate()
        .map(|(index, ballot)| {
            let siblings = tree::proof(&leaves, index).unwrap_or_default();
            Receipt::new(ballot, index, &siblings)
        })
        .collect();
    Ok((record, receipts))
}

/// Count one vote per ballot. A choice outside the option list is an error,
/// so a malformed ballot can never silently inflate another choice.
fn tally(options: &[String], ballots: &[Ballot]) -> Result<Vec<u64>> {
    let mut counts = vec![0u64; options.len()];
    for ballot in ballots {
        let slot = counts.get_mut(ballot.choice as usize).ok_or_else(|| {
            AtmosphericError::Message(format!(
                "choice {} is outside the poll's {} options",
                ballot.choice,
                options.len()
            ))
        })?;
        *slot += 1;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ballots(poll_id: &str, choices: &[u16]) -> Vec<Ballot> {
        choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let salt = [index as u8 + 1; 32];
                Ballot::new(poll_id, &format!("did:plc:voter{index}"), *choice, salt)
            })
            .collect()
    }

    fn options() -> Vec<String> {
        vec!["yes".into(), "no".into()]
    }

    #[test]
    fn publish_signs_a_record_and_every_receipt_verifies() {
        let key = crate::signing::generate();
        let (record, receipts) = publish(
            "Ship it?".into(),
            options(),
            "2026-11-26T00:00:00Z".into(),
            "2026-11-25T00:00:00Z".into(),
            &ballots("seed", &[0, 1, 0, 1, 0]),
            &key,
        )
        .unwrap();
        record.verify().unwrap();
        assert_eq!(record.tally, vec![3, 2]);
        assert_eq!(record.participants.len(), 5);
        assert_eq!(receipts.len(), 5);
        for receipt in &receipts {
            receipt.verify(&record).unwrap();
        }
    }

    #[test]
    fn receipts_round_trip_through_json() {
        let key = crate::signing::generate();
        let (record, receipts) = publish(
            "Ship it?".into(),
            options(),
            "2026-11-26T00:00:00Z".into(),
            "2026-11-25T00:00:00Z".into(),
            &ballots("seed", &[0, 1, 1]),
            &key,
        )
        .unwrap();
        let json = serde_json::to_string(&receipts[1]).unwrap();
        let back: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, receipts[1]);
        back.verify(&record).unwrap();
    }

    #[test]
    fn a_receipt_from_another_poll_does_not_verify() {
        let key = crate::signing::generate();
        let (record, receipts) = publish(
            "One".into(),
            options(),
            "2026-11-26T00:00:00Z".into(),
            "2026-11-25T00:00:00Z".into(),
            &ballots("seed", &[0, 1]),
            &key,
        )
        .unwrap();
        let mut other = record.clone();
        other.question = "Two".into();
        other.sign(&key);
        assert!(receipts[0].verify(&other).is_err());
    }

    #[test]
    fn tampering_with_a_receipt_fails() {
        let key = crate::signing::generate();
        let (record, receipts) = publish(
            "Ship it?".into(),
            options(),
            "2026-11-26T00:00:00Z".into(),
            "2026-11-25T00:00:00Z".into(),
            &ballots("seed", &[0, 1, 0]),
            &key,
        )
        .unwrap();
        let mut flipped = receipts[0].clone();
        flipped.choice = 1;
        assert!(flipped.verify(&record).is_err());
        let mut moved = receipts[0].clone();
        moved.index = 1;
        assert!(moved.verify(&record).is_err());
    }

    #[test]
    fn publishing_needs_votes_and_valid_choices() {
        let key = crate::signing::generate();
        assert!(
            publish(
                "q".into(),
                options(),
                "2026-11-26T00:00:00Z".into(),
                "2026-11-25T00:00:00Z".into(),
                &[],
                &key,
            )
            .is_err()
        );

        let bad = vec![Ballot::new("seed", "did:plc:x", 9, [1u8; 32])];
        assert!(
            publish(
                "q".into(),
                options(),
                "2026-11-26T00:00:00Z".into(),
                "2026-11-25T00:00:00Z".into(),
                &bad,
                &key,
            )
            .is_err()
        );
    }

    #[test]
    fn fresh_salt_is_not_all_zero() {
        assert_ne!(Ballot::fresh_salt(), [0u8; 32]);
    }
}
