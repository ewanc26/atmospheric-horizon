//! Deterministic hashing for Counting Stones ballots (#193).
//!
//! A voter's private receipt holds `(did, choice, salt)`. Only the
//! commitment to the choice and a per-poll nullifier for the DID enter the
//! Merkle leaf, so the published root never pairs a DID with a choice. Every
//! hash is domain-separated and length-prefixed, so a value from one field
//! cannot be reinterpreted as another.
//!
//! The tree is a balanced binary Merkle tree: odd levels duplicate their last
//! leaf, which keeps proofs independent of sibling count. An empty ballot box
//! has no root, so a poll cannot be published without at least one vote.

use sha2::{Digest, Sha256};

/// Domain separator for the commitment to a choice.
const DOMAIN_COMMITMENT: &[u8] = b"atmospheric-poll/commitment/v1";
/// Domain separator for the one-vote-per-DID nullifier.
const DOMAIN_NULLIFIER: &[u8] = b"atmospheric-poll/nullifier/v1";
/// Domain separator for a Merkle leaf.
const DOMAIN_LEAF: &[u8] = b"atmospheric-poll/leaf/v1";
/// Domain separator for an internal Merkle node.
const DOMAIN_NODE: &[u8] = b"atmospheric-poll/node/v1";

/// Hash a domain plus a list of length-prefixed byte parts. The length prefix
/// makes every concatenation unambiguous: `("ab", "c")` and `("a", "bc")`
/// produce different digests.
pub fn hash_parts(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u32).to_be_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u32).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Commit to a choice index with a fresh 32-byte salt. The commitment hides
/// the choice until the salt is revealed, so it can sit in a public leaf.
pub fn commitment(choice: u16, salt: &[u8; 32]) -> [u8; 32] {
    hash_parts(DOMAIN_COMMITMENT, &[&(choice as u32).to_be_bytes(), salt])
}

/// A deterministic per-poll nullifier for a DID. Two ballots from the same
/// DID in one poll share a nullifier, which lets a verifier detect that the
/// server admitted a duplicate DID even though choices stay hidden.
pub fn nullifier(poll_id: &str, did: &str) -> [u8; 32] {
    hash_parts(DOMAIN_NULLIFIER, &[poll_id.as_bytes(), did.as_bytes()])
}

/// The leaf committed to the Merkle tree: the DID, its hidden choice and the
/// one-vote nullifier. The choice never appears in plaintext.
pub fn leaf(did: &str, commitment: &[u8; 32], nullifier: &[u8; 32]) -> [u8; 32] {
    hash_parts(DOMAIN_LEAF, &[did.as_bytes(), commitment, nullifier])
}

/// Hash an internal node from its two children.
pub fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    hash_parts(DOMAIN_NODE, &[left, right])
}

/// Lower-hex encoding of a 32-byte digest.
pub fn hex(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Decode exactly 64 lower-hex characters into a 32-byte digest. Upper-case
/// hex is deliberately rejected, matching the record's canonical form.
pub fn from_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let high = (chunk[0] as char).to_digit(16)?;
        let low = (chunk[1] as char).to_digit(16)?;
        if chunk[0].is_ascii_uppercase() || chunk[1].is_ascii_uppercase() {
            return None;
        }
        out[index] = ((high << 4) | low) as u8;
    }
    Some(out)
}

/// The one level of tree reduction, used by both the root and proof builders.
fn reduce(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    for pair in level.chunks(2) {
        let left = pair[0];
        let right = pair.get(1).copied().unwrap_or(left);
        next.push(node(&left, &right));
    }
    next
}

/// The Merkle root of the ballots in submission order, or `None` when the box
/// is empty.
pub fn root(leaves: &[[u8; 32]]) -> Option<[u8; 32]> {
    let mut level = leaves.to_vec();
    if level.is_empty() {
        return None;
    }
    while level.len() > 1 {
        level = reduce(&level);
    }
    Some(level[0])
}

/// The sibling hashes a voter needs to prove their leaf is in the tree. The
/// last leaf of an odd level is paired with itself, so a proof is always the
/// same length for a given ballot count.
pub fn proof(leaves: &[[u8; 32]], index: usize) -> Option<Vec<[u8; 32]>> {
    if index >= leaves.len() {
        return None;
    }
    let mut level = leaves.to_vec();
    let mut position = index;
    let mut siblings = Vec::new();
    while level.len() > 1 {
        let sibling = if position.is_multiple_of(2) {
            position + 1
        } else {
            position - 1
        };
        siblings.push(*level.get(sibling).unwrap_or(&level[position]));
        level = reduce(&level);
        position /= 2;
    }
    Some(siblings)
}

/// The number of sibling hashes a proof for `leaf_count` leaves must carry.
fn proof_len(leaf_count: usize) -> usize {
    let mut levels = 0;
    let mut count = leaf_count;
    while count > 1 {
        levels += 1;
        count = count.div_ceil(2);
    }
    levels
}

/// Verify that `leaf` at `index` commits to `root` in a tree of `leaf_count`
/// leaves. Both the proof length and the index are checked, so a short proof
/// or an out-of-range position cannot be replayed.
pub fn verify(
    root: &[u8; 32],
    leaf: &[u8; 32],
    index: usize,
    siblings: &[[u8; 32]],
    leaf_count: usize,
) -> bool {
    if leaf_count == 0 || index >= leaf_count || siblings.len() != proof_len(leaf_count) {
        return false;
    }
    let mut current = *leaf;
    let mut position = index;
    for sibling in siblings {
        current = if position.is_multiple_of(2) {
            node(&current, sibling)
        } else {
            node(sibling, &current)
        };
        position /= 2;
    }
    current == *root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(count: usize) -> Vec<[u8; 32]> {
        (0..count)
            .map(|index| Sha256::digest((index as u64).to_be_bytes()).into())
            .collect()
    }

    #[test]
    fn hash_parts_separates_fields() {
        assert_ne!(
            hash_parts(DOMAIN_LEAF, &[b"ab", b"c"]),
            hash_parts(DOMAIN_LEAF, &[b"a", b"bc"])
        );
        assert_ne!(
            hash_parts(DOMAIN_LEAF, &[b"a"]),
            hash_parts(DOMAIN_NODE, &[b"a"])
        );
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        let digest = leaves(1).remove(0);
        assert_eq!(from_hex(&hex(&digest)), Some(digest));
        assert_eq!(from_hex(""), None);
        assert_eq!(from_hex(&"A".repeat(64)), None);
        assert_eq!(from_hex(&"a".repeat(63)), None);
        assert_eq!(from_hex(&format!("{}z", "a".repeat(63))), None);
    }

    #[test]
    fn empty_tree_has_no_root() {
        assert_eq!(root(&[]), None);
        assert_eq!(proof(&[], 0), None);
    }

    #[test]
    fn single_leaf_is_its_own_root() {
        let items = leaves(1);
        assert_eq!(root(&items), Some(items[0]));
        assert_eq!(proof(&items, 0), Some(vec![]));
        let root_bytes = root(&items).unwrap();
        assert!(verify(&root_bytes, &items[0], 0, &[], 1));
        assert!(!verify(&root_bytes, &items[0], 1, &[], 1));
    }

    #[test]
    fn every_leaf_verifies_in_odd_and_even_trees() {
        for count in [2, 3, 4, 5, 7, 9] {
            let items = leaves(count);
            let root_bytes = root(&items).unwrap();
            for (index, leaf) in items.iter().enumerate() {
                let siblings = proof(&items, index).unwrap();
                assert_eq!(siblings.len(), proof_len(count));
                assert!(verify(&root_bytes, leaf, index, &siblings, count));
                // A wrong leaf, position or sibling fails.
                let wrong = [0u8; 32];
                assert!(!verify(&root_bytes, &wrong, index, &siblings, count));
                assert!(!verify(
                    &root_bytes,
                    leaf,
                    (index + 1) % count,
                    &siblings,
                    count
                ));
                let mut broken = siblings.clone();
                if !broken.is_empty() {
                    broken[0] = wrong;
                    assert!(!verify(&root_bytes, leaf, index, &broken, count));
                }
            }
        }
    }

    #[test]
    fn root_is_order_sensitive() {
        let items = leaves(4);
        let mut reordered = items.clone();
        reordered.swap(0, 1);
        assert_ne!(root(&items), root(&reordered));
    }
}
