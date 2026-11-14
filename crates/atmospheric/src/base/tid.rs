//! AT Protocol TIDs (timestamp identifiers) for record keys.
//!
//! A TID packs a 53-bit microsecond UNIX timestamp and a 10-bit clock
//! identifier into a 63-bit integer, encoded big-endian in the sortable
//! base32 alphabet `234567abcdefghijklmnopqrstuvwxyz`. The alphabet is
//! ASCII-ascending and every key is 13 characters, so lexicographic order of
//! the keys matches creation order — records keyed by TID accumulate as a
//! naturally time-sorted, append-only history (this is what atpcraft uses for
//! its `at.atpcraft.player.chat` records).

/// The sortable base32 alphabet (digits before letters, both ascending).
const S32: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// Encode a 63-bit value as a 13-character TID string.
fn encode(value: u64) -> String {
    debug_assert!(value < (1 << 63));
    let mut out = String::with_capacity(13);
    for shift in (0..65).step_by(5).rev() {
        out.push(S32[(value >> shift) as usize & 0x1f] as char);
    }
    out
}

/// Decode a 13-character TID string back into its 63-bit value. Used by tests
/// (and available to callers that need to sort or compare TIDs numerically).
pub fn decode(tid: &str) -> Option<u64> {
    if tid.len() != 13 {
        return None;
    }
    let mut value = 0u64;
    for byte in tid.bytes() {
        let digit = S32.iter().position(|&c| c == byte)? as u64;
        if value >= (1 << 58) {
            return None; // would overflow the 63-bit TID space
        }
        value = (value << 5) | digit;
    }
    Some(value)
}

/// A monotonic TID generator: one per worker, fixed random clock id.
pub struct TidGen {
    clock_id: u64,
    last_micros: u64,
}

impl TidGen {
    /// A new generator. The clock id is derived from the current time and the
    /// process id (std has no RNG), which is unique enough within one process
    /// as long as one generator is used — the worker owns the only one.
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            clock_id: (nanos ^ (std::process::id() as u64) << 20) & 0x3ff,
            last_micros: 0,
        }
    }

    /// The next TID, strictly greater than every TID this generator has
    /// returned (monotonic even if the clock stands still).
    pub fn next_tid(&mut self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        self.next_at(now)
    }

    /// The next TID for an explicit microsecond timestamp, keeping the
    /// generator monotonic.
    pub fn next_at(&mut self, micros: u64) -> String {
        let micros = micros.max(self.last_micros + 1);
        self.last_micros = micros;
        encode((micros << 10) | self.clock_id)
    }
}

impl Default for TidGen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_zero_is_all_twos() {
        // '2' is digit 0 in the sortable alphabet.
        assert_eq!(encode(0), "2222222222222");
    }

    #[test]
    fn tids_are_13_chars_from_the_sortable_alphabet() {
        let mut tid_gen = TidGen {
            clock_id: 0x2a,
            last_micros: 0,
        };
        let tid = tid_gen.next_at(1_700_000_000_000_000);
        assert_eq!(tid.len(), 13);
        assert!(tid.bytes().all(|b| S32.contains(&b)));
    }

    #[test]
    fn tids_round_trip_through_decode() {
        let mut tid_gen = TidGen {
            clock_id: 1023,
            last_micros: 0,
        };
        let tid = tid_gen.next_at(1_700_000_000_000_000);
        let value = decode(&tid).unwrap();
        assert_eq!(value, (1_700_000_000_000_000u64 << 10) | 1023);
        assert_eq!(encode(value), tid);
    }

    #[test]
    fn tids_are_strictly_monotonic_and_sortable() {
        let mut tid_gen = TidGen {
            clock_id: 7,
            last_micros: 0,
        };
        let mut tids = Vec::new();
        // Same timestamp repeatedly: the generator must still advance.
        for _ in 0..100 {
            tids.push(tid_gen.next_at(1_700_000_000_000_000));
        }
        for pair in tids.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{} must sort before {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn encoding_preserves_numeric_order() {
        let a = encode((1_700_000_000_000_000u64 << 10) | 1);
        let b = encode(1_700_000_000_000_001u64 << 10);
        assert!(a < b);
        assert_eq!(decode("2222222222222"), Some(0));
        assert!(decode("222222222222").is_none(), "12 chars is not a TID");
        assert!(decode("!!!!!!!!!!!!!").is_none(), "non-alphabet chars");
    }
}
