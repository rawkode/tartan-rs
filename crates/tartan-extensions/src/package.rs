use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EXTENSION_API;

/// The parts of an extension's `tartan.json` this host reads. Other fields are kept in
/// [`Manifest::raw`] for the forge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The extension's id, such as `rawkode.issues`.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The package version (semver).
    pub version: String,
    /// The extension API the component targets.
    pub api: String,
    /// Where the extension's data lives.
    pub storage: Storage,
    /// The events the extension receives through `on-event`.
    #[serde(default)]
    pub subscribe: Vec<Subscription>,
    /// The whole manifest as published.
    #[serde(skip)]
    pub raw: serde_json::Value,
}

/// An extension's storage declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storage {
    /// One database per repository or per org.
    pub scope: StorageScope,
    /// Migration files, relative to the package root, applied in order.
    #[serde(default)]
    pub migrations: Vec<String>,
}

/// The unit an installation keeps one database for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageScope {
    /// One database per repository.
    Repo,
    /// One database per org.
    Org,
}

/// An event subscription: an event type pattern such as `ref.*`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The pattern.
    pub event: String,
}

/// A packaged extension: its manifest, its component and its migrations.
#[derive(Clone, Debug)]
pub struct ExtensionPackage {
    /// The manifest.
    pub manifest: Manifest,
    /// The component's bytes.
    pub component: Vec<u8>,
    /// Each migration's path and SQL, in order.
    pub migrations: Vec<(String, String)>,
}

/// Why a package was refused.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// A file could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// Why.
        source: std::io::Error,
    },
    /// The manifest is invalid.
    #[error("tartan.json: {0}")]
    Manifest(String),
}

/// The component's file name in a package directory.
pub const COMPONENT_FILE: &str = "component.wasm";

impl ExtensionPackage {
    /// Reads a package directory: `tartan.json`, `component.wasm` and the manifest's migrations.
    pub fn from_dir(dir: &Path) -> Result<Self, PackageError> {
        let read = |name: &str| {
            std::fs::read(dir.join(name)).map_err(|source| PackageError::Io {
                path: dir.join(name).display().to_string(),
                source,
            })
        };
        let manifest = read("tartan.json")?;
        let component = read(COMPONENT_FILE)?;
        let manifest = Manifest::parse(&manifest)?;
        let mut migrations = Vec::new();
        for path in &manifest.storage.migrations {
            let bytes = read(path)?;
            let script = String::from_utf8(bytes)
                .map_err(|_| PackageError::Manifest(format!("{path} is not UTF-8")))?;
            migrations.push((path.clone(), script));
        }
        Ok(Self {
            manifest,
            component,
            migrations,
        })
    }

    /// The SHA-256 of the manifest, the component and the migrations, hex-encoded. Each part is
    /// length-prefixed, so no two packages share a digest by shifting bytes between parts.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        let mut part = |bytes: &[u8]| {
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        };
        part(self.manifest.raw.to_string().as_bytes());
        part(&self.component);
        for (path, script) in &self.migrations {
            part(path.as_bytes());
            part(script.as_bytes());
        }
        hex::encode(hasher.finalize())
    }
}

impl Manifest {
    /// Parses and checks a `tartan.json`.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        let raw: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        let mut manifest: Manifest = serde_json::from_value(raw.clone())
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        manifest.raw = raw;
        if manifest.api != EXTENSION_API {
            return Err(PackageError::Manifest(format!(
                "api {:?} is not {EXTENSION_API}",
                manifest.api
            )));
        }
        let id_ok = !manifest.id.is_empty()
            && manifest.id.len() <= 64
            && manifest.id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
            });
        if !id_ok {
            return Err(PackageError::Manifest(format!("id {:?}", manifest.id)));
        }
        for path in &manifest.storage.migrations {
            let safe = !path.starts_with('/') && !path.split('/').any(|part| part == "..");
            if !safe {
                return Err(PackageError::Manifest(format!("migration path {path:?}")));
            }
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_fields_the_host_reads() {
        let manifest = Manifest::parse(
            br#"{"schema":1,"id":"rawkode.issues","name":"Issues","version":"0.4.0",
                "api":"tartan:ext@0.1.0","runtime":"wasm",
                "storage":{"scope":"repo","migrations":["migrations/0001_init.sql"]},
                "subscribe":[{"event":"ref.*"}]}"#,
        )
        .expect("valid");
        assert_eq!(manifest.storage.scope, StorageScope::Repo);
        assert_eq!(manifest.subscribe[0].event, "ref.*");
        assert_eq!(manifest.raw["runtime"], "wasm");
    }

    #[test]
    fn refuses_other_apis_and_escaping_paths() {
        let other = br#"{"id":"a","name":"A","version":"1.0.0","api":"tartan:ext@9",
            "storage":{"scope":"repo"}}"#;
        assert!(Manifest::parse(other).is_err());
        let escaping = br#"{"id":"a","name":"A","version":"1.0.0","api":"tartan:ext@0.1.0",
            "storage":{"scope":"repo","migrations":["../x.sql"]}}"#;
        assert!(Manifest::parse(escaping).is_err());
    }
}
