use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EXTENSION_API;

/// The parts of an extension's `tartan.json` this host reads. Other fields are kept in
/// [`Manifest::raw`] for the forge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The extension's id, such as `issues.tartan.rawkode.academy`.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The package version (semver).
    pub version: String,
    /// The extension API the component targets.
    pub api: String,
    /// Where the extension's data lives.
    pub storage: Storage,
    /// The extensions this one needs installed beside it on the same repository, by id. A
    /// configuration installing this package without them is refused.
    #[serde(default)]
    pub requires: Vec<String>,
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
    /// One database per repository: the only scope, since every extension installs on
    /// repositories.
    pub scope: StorageScope,
    /// Migration files, relative to the package root, applied in order.
    #[serde(default)]
    pub migrations: Vec<String>,
}

/// The unit an installation keeps one database for. Every extension installs on repositories,
/// so a manifest declaring any other scope does not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageScope {
    /// One database per repository.
    Repo,
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
    /// The package's files do not match its manifest.
    #[error("{0}")]
    Files(String),
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

    /// Reads a package from a CUE module's files, by path: `tartan.json`, `component.wasm` and
    /// the manifest's migrations. Other files belong to the module, but every `.sql` file must
    /// be one of the manifest's migrations, so none is silently left out of the package.
    pub fn from_module_files(files: &BTreeMap<String, Vec<u8>>) -> Result<Self, PackageError> {
        let read = |path: &str| {
            files
                .get(path)
                .cloned()
                .ok_or_else(|| PackageError::Files(format!("the module has no {path}")))
        };
        let manifest = Manifest::parse(&read("tartan.json")?)?;
        let component = read(COMPONENT_FILE)?;
        let unlisted: Vec<&String> = files
            .keys()
            .filter(|path| path.ends_with(".sql") && !manifest.storage.migrations.contains(path))
            .collect();
        if !unlisted.is_empty() {
            return Err(PackageError::Files(format!(
                "migrations {unlisted:?} are not in the manifest's {:?}",
                manifest.storage.migrations
            )));
        }
        let mut migrations = Vec::new();
        for path in &manifest.storage.migrations {
            let script = String::from_utf8(read(path)?)
                .map_err(|_| PackageError::Files(format!("{path} is not UTF-8")))?;
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
        if !valid_id(&manifest.id) {
            return Err(PackageError::Manifest(format!("id {:?}", manifest.id)));
        }
        for (index, required) in manifest.requires.iter().enumerate() {
            if !valid_id(required)
                || *required == manifest.id
                || manifest.requires[..index].contains(required)
            {
                return Err(PackageError::Manifest(format!(
                    "requires {required:?}: an extension id other than the package's own, \
                     listed once"
                )));
            }
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

/// Whether `id` is an extension id: 1 to 64 lowercase letters, digits, `.`, `-` and `_`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_fields_the_host_reads() {
        let manifest = Manifest::parse(
            br#"{"schema":1,"id":"issues.tartan.rawkode.academy","name":"Issues","version":"0.4.0",
                "api":"tartan:ext@0.1.0","runtime":"wasm",
                "storage":{"scope":"repo","migrations":["migrations/0001_init.sql"]},
                "subscribe":[{"event":"ref.*"}]}"#,
        )
        .expect("valid");
        assert_eq!(manifest.storage.scope, StorageScope::Repo);
        assert!(manifest.requires.is_empty());
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

    #[test]
    fn every_package_installs_on_repositories() {
        let org = br#"{"id":"a","name":"A","version":"1.0.0","api":"tartan:ext@0.1.0",
            "storage":{"scope":"org"}}"#;
        let error = Manifest::parse(org).expect_err("org scope");
        assert!(error.to_string().contains("repo"), "{error}");
    }

    #[test]
    fn requires_names_other_extensions_once() {
        let manifest = |requires: &str| {
            format!(
                r#"{{"id":"kanban.tartan.rawkode.academy","name":"K","version":"1.0.0","api":"tartan:ext@0.1.0",
                "storage":{{"scope":"repo"}},"requires":{requires}}}"#
            )
        };
        let parsed = Manifest::parse(manifest(r#"["issues.tartan.rawkode.academy"]"#).as_bytes())
            .expect("valid");
        assert_eq!(parsed.requires, vec!["issues.tartan.rawkode.academy"]);
        for invalid in [
            r#"["kanban.tartan.rawkode.academy"]"#,
            r#"["issues.tartan.rawkode.academy","issues.tartan.rawkode.academy"]"#,
            r#"["Not An Id"]"#,
            r#""issues.tartan.rawkode.academy""#,
        ] {
            assert!(
                Manifest::parse(manifest(invalid).as_bytes()).is_err(),
                "{invalid}"
            );
        }
    }

    fn module(manifest: &str, extra: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        let mut files: BTreeMap<String, Vec<u8>> = [
            ("tartan.json", manifest),
            ("component.wasm", "\0asm"),
            ("cue.mod/module.cue", "module: \"example.com/x@v0\""),
            ("x.cue", "package x"),
        ]
        .iter()
        .map(|(p, c)| ((*p).to_owned(), c.as_bytes().to_vec()))
        .collect();
        files.extend(
            extra
                .iter()
                .map(|(p, c)| ((*p).to_owned(), c.as_bytes().to_vec())),
        );
        files
    }

    #[test]
    fn reads_a_module_and_refuses_unlisted_or_missing_migrations() {
        let manifest = r#"{"id":"a","name":"A","version":"1.0.0","api":"tartan:ext@0.1.0",
            "storage":{"scope":"repo","migrations":["migrations/0001.sql","migrations/0002.sql"]}}"#;
        let both = [
            ("migrations/0002.sql", "two"),
            ("migrations/0001.sql", "one"),
        ];
        let package = ExtensionPackage::from_module_files(&module(manifest, &both)).expect("valid");
        let paths: Vec<&str> = package.migrations.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["migrations/0001.sql", "migrations/0002.sql"]);

        let missing = module(manifest, &both[..1]);
        assert!(ExtensionPackage::from_module_files(&missing).is_err());
        let extra = module(
            manifest,
            &[both[0], both[1], ("migrations/0003.sql", "three")],
        );
        assert!(ExtensionPackage::from_module_files(&extra).is_err());
    }
}
