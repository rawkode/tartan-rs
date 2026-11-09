//! A repository's signing rule: the top-level `signing` of its configuration, with the shape of
//! `#Signing` in `tartan.rawkode.academy/types@v0.2.0`.
//!
//! ```cue
//! signing: types.#Signing & {require: [{
//!     refs:    ["refs/heads/main", "refs/tags/**"]
//!     signers: ["did:plc:abcdefghijklmnopqrstuvwx"]
//! }]}
//! ```
//!
//! A ref is *covered* when at least one entry's `refs` matches it. An object on a covered ref
//! satisfies the rule when it is verified and, for every matching entry that lists `signers`,
//! its signer is one of them; an entry without `signers` accepts any proved DID.
//!
//! In a ref pattern, `*` matches within one segment (`refs/heads/release-*`), and `**`, allowed
//! only as the whole last segment, matches one or more remaining segments (`refs/tags/**`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ConfigError;
use crate::did::Did;

/// The longest ref pattern accepted, in bytes.
const MAX_PATTERN_BYTES: usize = 1024;
/// The most entries one rule may hold.
const MAX_ENTRIES: usize = 100;
/// The most refs or signers one entry may list.
const MAX_LIST: usize = 1000;

/// A signing rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningRule {
    /// The entries, in the order written.
    pub require: Vec<Requirement>,
}

/// One entry of a rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// The refs it covers: full ref names or patterns.
    pub refs: Vec<String>,
    /// The DIDs allowed to sign them; `None` accepts any proved DID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signers: Option<Vec<Did>>,
}

/// What a rule asks of one ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// No entry covers the ref.
    Uncovered,
    /// The ref is covered. `allowed` is the set of DIDs every matching entry with `signers`
    /// admits (their intersection), or `None` when no matching entry lists `signers`.
    Covered {
        /// The DIDs allowed to sign, if restricted.
        allowed: Option<BTreeSet<String>>,
    },
}

impl Coverage {
    /// Whether the ref is covered.
    pub fn is_covered(&self) -> bool {
        matches!(self, Coverage::Covered { .. })
    }

    /// Whether `did` may sign the ref: any DID on a covered ref without `signers`, and any DID
    /// at all on an uncovered ref.
    pub fn allows(&self, did: &str) -> bool {
        match self {
            Coverage::Uncovered | Coverage::Covered { allowed: None } => true,
            Coverage::Covered {
                allowed: Some(allowed),
            } => allowed.contains(did),
        }
    }
}

impl SigningRule {
    /// What the rule asks of `ref_name`.
    pub fn coverage(&self, ref_name: &str) -> Coverage {
        let mut covered = false;
        let mut allowed: Option<BTreeSet<String>> = None;
        for entry in &self.require {
            if !entry.refs.iter().any(|p| ref_matches(p, ref_name)) {
                continue;
            }
            covered = true;
            if let Some(signers) = &entry.signers {
                let these: BTreeSet<String> = signers.iter().map(|d| d.to_string()).collect();
                allowed = Some(match allowed {
                    None => these,
                    Some(before) => before.intersection(&these).cloned().collect(),
                });
            }
        }
        if covered {
            Coverage::Covered { allowed }
        } else {
            Coverage::Uncovered
        }
    }

    /// Every DID the rule names, de-duplicated.
    pub fn signers(&self) -> BTreeSet<String> {
        self.require
            .iter()
            .flat_map(|entry| entry.signers.iter().flatten())
            .map(|did| did.to_string())
            .collect()
    }

    /// The rule as stored: its normalised JSON, which is also how two rules are compared.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Reads a stored rule.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

/// Whether the ref pattern `pattern` matches the full ref name `ref_name`.
pub fn ref_matches(pattern: &str, ref_name: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let name: Vec<&str> = ref_name.split('/').collect();
    match pattern.split_last() {
        Some((&"**", prefix)) => {
            name.len() > prefix.len()
                && prefix.iter().zip(&name).all(|(p, n)| segment_matches(p, n))
        }
        _ => {
            pattern.len() == name.len()
                && pattern
                    .iter()
                    .zip(&name)
                    .all(|(p, n)| segment_matches(p, n))
        }
    }
}

/// Glob match of one segment, where `*` matches any run of characters.
fn segment_matches(pattern: &str, segment: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == segment;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if segment.len() < first.len() + last.len()
        || !segment.starts_with(first)
        || !segment.ends_with(last)
    {
        return false;
    }
    let mut rest = &segment[first.len()..segment.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    true
}

/// Whether `pattern` is a valid ref pattern: `refs/` and one or more segments without spaces,
/// none empty, with `**` only as the whole last segment.
fn valid_pattern(pattern: &str) -> Result<(), String> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("is longer than {MAX_PATTERN_BYTES} bytes"));
    }
    let Some(rest) = pattern.strip_prefix("refs/") else {
        return Err("does not start with refs/".into());
    };
    let segments: Vec<&str> = rest.split('/').collect();
    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            return Err("has an empty segment".into());
        }
        if segment.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("contains a space or control character".into());
        }
        if segment.contains("**") && (*segment != "**" || i + 1 != segments.len()) {
            return Err("uses ** other than as the whole last segment".into());
        }
    }
    Ok(())
}

/// Checks and reads the exported `signing` value.
pub(crate) fn check(value: Value) -> Result<SigningRule, ConfigError> {
    let invalid = |message: String| Err(ConfigError::Invalid(message));
    let Value::Object(mut signing) = value else {
        return invalid("signing is not a struct".into());
    };
    let entries = match signing.remove("require") {
        None => Vec::new(),
        Some(Value::Array(entries)) => entries,
        Some(_) => return invalid("signing.require is not a list".into()),
    };
    if let Some(key) = signing.keys().next() {
        return invalid(format!(
            "signing.{key} is not a field of #Signing: it has only require"
        ));
    }
    if entries.len() > MAX_ENTRIES {
        return invalid(format!(
            "signing.require has more than {MAX_ENTRIES} entries"
        ));
    }
    let mut rule = SigningRule::default();
    for (i, entry) in entries.into_iter().enumerate() {
        let at = format!("signing.require[{i}]");
        let Value::Object(mut entry) = entry else {
            return invalid(format!("{at} is not a struct"));
        };
        let refs = match entry.remove("refs") {
            Some(Value::Array(refs)) if !refs.is_empty() => refs,
            Some(Value::Array(_)) => return invalid(format!("{at}.refs is empty")),
            None => return invalid(format!("{at} has no refs")),
            Some(_) => return invalid(format!("{at}.refs is not a list")),
        };
        if refs.len() > MAX_LIST {
            return invalid(format!("{at}.refs has more than {MAX_LIST} entries"));
        }
        let mut patterns = Vec::with_capacity(refs.len());
        for (j, pattern) in refs.into_iter().enumerate() {
            let Value::String(pattern) = pattern else {
                return invalid(format!("{at}.refs[{j}] is not a string"));
            };
            if let Err(why) = valid_pattern(&pattern) {
                return invalid(format!("{at}.refs[{j}] {pattern:?} {why}"));
            }
            patterns.push(pattern);
        }
        let signers = match entry.remove("signers") {
            None => None,
            Some(Value::Array(signers)) if !signers.is_empty() => {
                if signers.len() > MAX_LIST {
                    return invalid(format!("{at}.signers has more than {MAX_LIST} entries"));
                }
                let mut dids = Vec::with_capacity(signers.len());
                for (j, signer) in signers.into_iter().enumerate() {
                    let Value::String(signer) = signer else {
                        return invalid(format!("{at}.signers[{j}] is not a string"));
                    };
                    match signer.parse::<Did>() {
                        Ok(did) => dids.push(did),
                        Err(why) => return invalid(format!("{at}.signers[{j}]: {why}")),
                    }
                }
                Some(dids)
            }
            Some(Value::Array(_)) => {
                return invalid(format!(
                    "{at}.signers is empty: list at least one DID, or leave it out"
                ));
            }
            Some(_) => return invalid(format!("{at}.signers is not a list")),
        };
        if let Some(key) = entry.keys().next() {
            return invalid(format!(
                "{at}.{key} is not a field of a rule: it has only refs and signers"
            ));
        }
        rule.require.push(Requirement {
            refs: patterns,
            signers,
        });
    }
    Ok(rule)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ALICE: &str = "did:plc:abcdefghijklmnopqrstuvwx";
    const BOB: &str = "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn matches_ref_patterns() {
        assert!(ref_matches("refs/heads/main", "refs/heads/main"));
        assert!(!ref_matches("refs/heads/main", "refs/heads/main2"));
        assert!(!ref_matches("refs/heads/main", "refs/heads/main/x"));
        assert!(ref_matches(
            "refs/heads/release-*",
            "refs/heads/release-1.2"
        ));
        assert!(ref_matches("refs/heads/release-*", "refs/heads/release-"));
        assert!(!ref_matches("refs/heads/release-*", "refs/heads/release/1"));
        assert!(ref_matches("refs/heads/*", "refs/heads/junk"));
        assert!(ref_matches("refs/heads/a*b*c", "refs/heads/aXbYc"));
        assert!(!ref_matches("refs/heads/a*b*c", "refs/heads/aXc"));
        assert!(ref_matches("refs/tags/**", "refs/tags/v1"));
        assert!(ref_matches("refs/tags/**", "refs/tags/team/v1"));
        assert!(!ref_matches("refs/tags/**", "refs/tags"));
        assert!(!ref_matches("refs/tags/**", "refs/heads/v1"));
    }

    #[test]
    fn checks_patterns() {
        for valid in [
            "refs/heads/main",
            "refs/tags/**",
            "refs/heads/release-*",
            "refs/*/main",
        ] {
            assert!(valid_pattern(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "heads/main",
            "refs/",
            "refs/heads//main",
            "refs/heads/ma in",
            "refs/**/main",
            "refs/heads/a**",
            "refs/heads/**x",
            "main",
        ] {
            assert!(valid_pattern(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn reads_a_valid_rule() {
        let rule = check(json!({"require": [
            {"refs": ["refs/heads/main", "refs/tags/**"], "signers": [ALICE]},
            {"refs": ["refs/heads/release-*"]},
        ]}))
        .expect("valid");
        assert_eq!(rule.require.len(), 2);
        assert_eq!(rule.signers(), BTreeSet::from([ALICE.to_owned()]));
        assert!(rule.coverage("refs/heads/main").allows(ALICE));
        assert!(!rule.coverage("refs/heads/main").allows(BOB));
        assert!(rule.coverage("refs/heads/release-2").allows(BOB));
        assert_eq!(rule.coverage("refs/heads/junk"), Coverage::Uncovered);
        assert_eq!(SigningRule::from_json(&rule.to_json()).expect("json"), rule);
    }

    #[test]
    fn matching_entries_with_signers_intersect() {
        let rule = check(json!({"require": [
            {"refs": ["refs/heads/main"], "signers": [ALICE, BOB]},
            {"refs": ["refs/heads/*"], "signers": [BOB]},
        ]}))
        .expect("valid");
        let main = rule.coverage("refs/heads/main");
        assert!(main.allows(BOB));
        assert!(!main.allows(ALICE));
    }

    #[test]
    fn refuses_bad_rules() {
        let refused = |value: Value| match check(value) {
            Err(ConfigError::Invalid(message)) => message,
            other => panic!("not refused: {other:?}"),
        };
        assert!(refused(json!({"requires": []})).contains("signing.requires"));
        assert!(refused(json!([])).contains("not a struct"));
        assert!(refused(json!({"require": [{}]})).contains("has no refs"));
        assert!(refused(json!({"require": [{"refs": []}]})).contains("empty"));
        assert!(refused(json!({"require": [{"refs": ["heads/main"]}]})).contains("refs/"));
        assert!(refused(json!({"require": [{"refs": ["refs/**/x"]}]})).contains("**"));
        assert!(
            refused(json!({"require": [{"refs": ["refs/heads/main"], "signers": ["alice.test"]}]}))
                .contains("signers[0]")
        );
        assert!(
            refused(json!({"require": [{"refs": ["refs/heads/main"], "signers": []}]}))
                .contains("empty")
        );
        assert!(
            refused(json!({"require": [{"refs": ["refs/heads/main"], "signer": [ALICE]}]}))
                .contains("signing.require[0].signer")
        );
    }
}
