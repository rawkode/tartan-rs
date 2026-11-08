use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The name of an org or repository in paths: 1 to 64 lowercase letters, digits and `-`, not
/// starting or ending with `-`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slug(String);

/// Why a string is not a [`Slug`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid slug {0:?}: use 1 to 64 lowercase letters, digits and -, not at either end")]
pub struct SlugError(String);

/// The longest slug, in bytes.
const SLUG_MAX: usize = 64;

impl Slug {
    /// The slug as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Slug {
    type Err = SlugError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let allowed = s
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let valid = !s.is_empty()
            && s.len() <= SLUG_MAX
            && allowed
            && !s.starts_with('-')
            && !s.ends_with('-');
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(SlugError(s.to_owned()))
        }
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Slug {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Slug {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_refuses() {
        for good in ["a", "acme", "my-repo-2"] {
            assert!(good.parse::<Slug>().is_ok(), "{good}");
        }
        let long = "a".repeat(SLUG_MAX + 1);
        for bad in ["", "-a", "a-", "A", "a_b", "a/b", long.as_str()] {
            assert!(bad.parse::<Slug>().is_err(), "{bad}");
        }
    }
}
