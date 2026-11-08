use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A pattern over dot-separated event types: `*` matches one token and a final `>` matches one
/// or more. `repo.*` matches `repo.ready`; `ref.>` matches `ref.updated`; `>` matches every event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventPattern(String);

/// Why a string is not an [`EventPattern`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid event pattern {0:?}")]
pub struct EventPatternError(String);

impl EventPattern {
    /// Whether the event type `kind` matches.
    pub fn matches(&self, kind: &str) -> bool {
        let mut tokens = kind.split('.');
        for part in self.0.split('.') {
            match (part, tokens.next()) {
                (">", Some(_)) => return true,
                ("*", Some(_)) => {}
                (literal, Some(token)) if literal == token => {}
                _ => return false,
            }
        }
        tokens.next().is_none()
    }

    /// The pattern as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for EventPattern {
    type Err = EventPatternError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        let last = parts.len() - 1;
        let valid_part = |(index, part): (usize, &&str)| match *part {
            ">" => index == last,
            "*" => true,
            literal => {
                !literal.is_empty()
                    && literal
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            }
        };
        if s.len() <= 200 && parts.iter().enumerate().all(valid_part) {
            Ok(Self(s.to_owned()))
        } else {
            Err(EventPatternError(s.to_owned()))
        }
    }
}

impl fmt::Display for EventPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for EventPattern {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EventPattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(s: &str) -> EventPattern {
        s.parse().expect("valid pattern")
    }

    #[test]
    fn matches_tokens_and_wildcards() {
        assert!(pattern(">").matches("ref.updated"));
        assert!(pattern("ref.>").matches("ref.updated"));
        assert!(!pattern("ref.>").matches("ref"));
        assert!(pattern("repo.*").matches("repo.ready"));
        assert!(!pattern("repo.*").matches("repo.ready.now"));
        assert!(!pattern("repo.*").matches("org.created"));
        assert!(pattern("push.accepted").matches("push.accepted"));
        assert!(!pattern("push.accepted").matches("push"));
    }

    #[test]
    fn refuses_malformed_patterns() {
        for bad in ["", "a..b", ">.a", "A.b", "a.b c"] {
            assert!(bad.parse::<EventPattern>().is_err(), "{bad}");
        }
    }
}
