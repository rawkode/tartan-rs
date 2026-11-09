//! TIDs: ATProto's timestamp identifiers, used as signature record keys.
//!
//! A TID is 13 characters of the sortable base32 alphabet `234567abcdefghijklmnopqrstuvwxyz`
//! encoding a 64-bit number whose top bit is zero: 53 bits of microseconds since the Unix epoch,
//! then a 10-bit clock identifier.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

const ALPHABET: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// A syntactically valid TID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tid(String);

/// Why a string is not a TID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a TID")]
pub struct TidError;

impl Tid {
    /// Parses a TID: 13 characters of the alphabet, the first from `234567abcdefghij` (so the
    /// top bit is zero).
    pub fn parse(value: &str) -> Result<Self, TidError> {
        let bytes = value.as_bytes();
        let valid = bytes.len() == 13
            && bytes.iter().all(|b| ALPHABET.contains(b))
            && b"234567abcdefghij".contains(&bytes[0]);
        if valid {
            Ok(Tid(value.to_owned()))
        } else {
            Err(TidError)
        }
    }

    /// The TID for `micros` since the epoch and `clock_id` (only its low 10 bits are used).
    pub fn from_parts(micros: u64, clock_id: u16) -> Self {
        let value = ((micros & ((1 << 53) - 1)) << 10) | u64::from(clock_id & 0x3ff);
        let mut out = [0u8; 13];
        for (i, slot) in out.iter_mut().enumerate() {
            let shift = 5 * (12 - i);
            *slot = ALPHABET[((value >> shift) & 31) as usize];
        }
        Tid(String::from_utf8_lossy(&out).into_owned())
    }

    /// A TID for now, with a random clock identifier.
    pub fn now() -> Self {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or_default();
        let mut clock = [0u8; 2];
        // A failure leaves clock id 0, which is still a valid TID.
        let _ = getrandom::fill(&mut clock);
        Tid::from_parts(micros, u16::from_le_bytes(clock))
    }

    /// Microseconds since the epoch.
    pub fn micros(&self) -> u64 {
        self.value() >> 10
    }

    fn value(&self) -> u64 {
        self.0.bytes().fold(0u64, |acc, b| {
            let digit = ALPHABET.iter().position(|&c| c == b).unwrap_or(0) as u64;
            (acc << 5) | digit
        })
    }

    /// The TID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Tid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let tid = Tid::from_parts(1_731_000_000_000_000, 513);
        assert_eq!(tid.as_str().len(), 13);
        assert_eq!(Tid::parse(tid.as_str()), Ok(tid.clone()));
        assert_eq!(tid.micros(), 1_731_000_000_000_000);
        let later = Tid::from_parts(1_731_000_000_000_001, 0);
        assert!(later > tid);
    }

    #[test]
    fn now_is_valid() {
        let tid = Tid::now();
        assert!(Tid::parse(tid.as_str()).is_ok());
    }

    #[test]
    fn validation() {
        for ok in ["3lbmxyz2abc2k", "2222222222222", "jzzzzzzzzzzzz"] {
            assert!(Tid::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "3lbmxyz2abc2",
            "3lbmxyz2abc2kk",
            "klbmxyz2abc2k",
            "3LBMXYZ2ABC2K",
            "3lbmxyz2abc1k",
            "self",
        ] {
            assert!(Tid::parse(bad).is_err(), "{bad}");
        }
    }
}
