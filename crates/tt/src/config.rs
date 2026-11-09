//! `$XDG_CONFIG_HOME/tt/config.toml` (design D16): the default forge, the default signing DID
//! and `sign.offline`. It never holds a secret.
//!
//! A default that changes unasked would silently redirect later commands, so only two places
//! write them, each only when the default is unset or `--set-default` is given:
//! [`Config::offer_forge`] (`tt login`) and [`Config::offer_signing_did`] (`tt sign --login`,
//! and `tt login`'s first-run signing login).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

/// What signing does with a record it cannot publish now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Offline {
    /// Queue the record and return the signature (the default).
    #[default]
    Queue,
    /// Fail the signing.
    Fail,
}

/// The `[sign]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignSection {
    /// `sign.offline`.
    #[serde(default)]
    pub offline: Offline,
}

/// The contents of `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFile {
    /// The forge `tt` talks to when no `--forge` or `TT_FORGE` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_forge: Option<String>,
    /// The DID signing uses when Git passes a key that is not a DID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_signing_did: Option<String>,
    /// Signing options.
    #[serde(default)]
    pub sign: SignSection,
}

/// `config.toml` and where it lives.
#[derive(Debug, Clone)]
pub struct Config {
    path: PathBuf,
    /// What the file holds.
    pub file: ConfigFile,
}

/// What [`Config::offer_forge`] or [`Config::offer_signing_did`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offered {
    /// The value became the default.
    Set,
    /// The default already was this value.
    Unchanged,
    /// Another default is kept.
    Kept(String),
}

impl Config {
    /// Reads `config.toml` from `dir` (an absent file is the empty configuration).
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("config.toml");
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigFile::default(),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Config { path, file })
    }

    fn save(&self) -> Result<()> {
        let text = toml::to_string(&self.file).context("encoding config.toml")?;
        paths::write_private(&self.path, text.as_bytes())
    }

    /// `tt login`'s rule: `forge` becomes the default forge when none is set or `set_default`.
    pub fn offer_forge(&mut self, forge: &str, set_default: bool) -> Result<Offered> {
        let offered = decide(self.file.default_forge.as_deref(), forge, set_default);
        if offered == Offered::Set {
            self.file.default_forge = Some(forge.to_owned());
            self.save()?;
        }
        Ok(offered)
    }

    /// `tt sign --login`'s rule: `did` becomes the default signing DID when none is set or
    /// `set_default`.
    pub fn offer_signing_did(&mut self, did: &str, set_default: bool) -> Result<Offered> {
        let offered = decide(self.file.default_signing_did.as_deref(), did, set_default);
        if offered == Offered::Set {
            self.file.default_signing_did = Some(did.to_owned());
            self.save()?;
        }
        Ok(offered)
    }
}

fn decide(current: Option<&str>, value: &str, set_default: bool) -> Offered {
    match current {
        Some(current) if current == value => Offered::Unchanged,
        Some(current) if !set_default => Offered::Kept(current.to_owned()),
        _ => Offered::Set,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_change_only_when_unset_or_asked() {
        let temp = tempfile::tempdir().expect("temp");
        let mut config = Config::load(temp.path()).expect("load");
        assert_eq!(config.file, ConfigFile::default());
        assert_eq!(
            config
                .offer_forge("https://forge.example", false)
                .expect("offer"),
            Offered::Set
        );
        assert_eq!(
            config
                .offer_forge("https://other.example", false)
                .expect("offer"),
            Offered::Kept("https://forge.example".into())
        );
        assert_eq!(
            config
                .offer_signing_did("did:plc:aaaaaaaaaaaaaaaaaaaaaaaa", false)
                .expect("offer"),
            Offered::Set
        );
        let reread = Config::load(temp.path()).expect("load");
        assert_eq!(
            reread.file.default_forge.as_deref(),
            Some("https://forge.example")
        );
        assert_eq!(
            reread.file.default_signing_did.as_deref(),
            Some("did:plc:aaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let mut config = reread;
        assert_eq!(
            config
                .offer_forge("https://other.example", true)
                .expect("offer"),
            Offered::Set
        );
        let reread = Config::load(temp.path()).expect("load");
        assert_eq!(
            reread.file.default_forge.as_deref(),
            Some("https://other.example")
        );
        assert_eq!(
            reread.file.default_signing_did.as_deref(),
            Some("did:plc:aaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let text = std::fs::read_to_string(temp.path().join("config.toml")).expect("read");
        assert!(!text.contains("tartan_"), "no secret in config.toml");
    }

    #[test]
    fn sign_offline_defaults_to_queue() {
        let file: ConfigFile = toml::from_str("[sign]\noffline = \"fail\"\n").expect("toml");
        assert_eq!(file.sign.offline, Offline::Fail);
        let file: ConfigFile = toml::from_str("").expect("toml");
        assert_eq!(file.sign.offline, Offline::Queue);
    }
}
