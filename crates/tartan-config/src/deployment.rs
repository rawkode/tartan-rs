//! The deployment's configuration: the CUE package `forge` in the file or directory named by
//! `TARTAN_CONFIG`, usually mounted from the deployment's config map.
//!
//! ```cue
//! package forge
//!
//! sources: "github.com/rawkode": "ghcr.io/rawkode/cue"
//! modules: [
//!     "tartan.dev/types@v0.1.0",
//!     "github.com/rawkode/tartan-ext/issues@v0.6.0",
//! ]
//! ```
//!
//! `modules` lists the CUE module versions the forge makes available, each a fully qualified
//! `path@vX.Y.Z`. `sources` maps module path prefixes to the OCI registries the forge copies
//! them from, written as `CUE_REGISTRY` writes a registry: `host[:port][/repository-prefix]`,
//! optionally with `+insecure` (plain HTTP) or `+secure`. A module comes from the source with
//! the longest prefix that matches whole path elements of its path.
//!
//! The package is evaluated natively, with no registry: it is the operator's file, not a
//! module's. A configuration that does not evaluate, has a key other than `modules` and
//! `sources`, lists a module that is not fully qualified or names no source for it is refused,
//! and the forge does not start.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// The CUE package a deployment's configuration is.
pub const PACKAGE: &str = "forge";

/// A deployment's configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deployment {
    /// The module versions the forge makes available, in the order listed.
    pub modules: Vec<ModuleVersion>,
    /// The registries modules are copied from, by module path prefix.
    pub sources: BTreeMap<String, Source>,
}

/// One version of a CUE module, such as `github.com/rawkode/tartan-ext/issues@v0.6.0`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleVersion {
    /// The module path, without a major version suffix: `github.com/rawkode/tartan-ext/issues`.
    pub path: String,
    /// The version, with its leading `v`: `v0.6.0`.
    pub version: String,
}

/// An OCI registry modules are copied from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The registry's host, with its port if it has one.
    pub host: String,
    /// The repository prefix the module path is appended to; empty for none.
    pub prefix: String,
    /// Whether the registry is reached over plain HTTP.
    pub insecure: bool,
}

/// Why a deployment's configuration was refused.
#[derive(Debug, thiserror::Error)]
pub enum DeploymentError {
    /// The configuration could not be read.
    #[error("{path}: {message}")]
    Read {
        /// The path that was read.
        path: String,
        /// Why it failed.
        message: String,
    },
    /// The CUE does not evaluate.
    #[error("the package {PACKAGE} does not evaluate: {0}")]
    Evaluation(String),
    /// The value has the wrong shape, or an entry is invalid.
    #[error("{0}")]
    Invalid(String),
}

static MODULE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    // The first element is a domain name; every element is lower case, as OCI repository names
    // and CUE module paths both require.
    Regex::new(r"^[a-z0-9]+([.-][a-z0-9]+)*\.[a-z]{2,}(/[a-z0-9]+([._-][a-z0-9]+)*)*$")
        .expect("valid regex")
});
static MODULE_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$")
        .expect("valid regex")
});
static PATH_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9]+([.-][a-z0-9]+)*(/[a-z0-9]+([._-][a-z0-9]+)*)*$").expect("valid regex")
});
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([a-zA-Z0-9]([a-zA-Z0-9.-]*[a-zA-Z0-9])?|\[[0-9a-fA-F:]+\])(:[0-9]{1,5})?$")
        .expect("valid regex")
});

impl ModuleVersion {
    /// The module's major version: `v0` for `v0.6.0`.
    pub fn major(&self) -> &str {
        self.version
            .split_once('.')
            .map_or(self.version.as_str(), |(major, _)| major)
    }
}

impl std::fmt::Display for ModuleVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.path, self.version)
    }
}

impl std::str::FromStr for ModuleVersion {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((path, version)) = text.split_once('@') else {
            return Err(format!("{text:?} has no version: write path@vX.Y.Z"));
        };
        if !MODULE_PATH.is_match(path) {
            return Err(format!(
                "{text:?} does not name a fully qualified module path"
            ));
        }
        if !MODULE_VERSION.is_match(version) {
            return Err(format!(
                "{text:?} does not name a version such as v1.2.0 (a major version alone is not one)"
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            version: version.to_owned(),
        })
    }
}

impl std::str::FromStr for Source {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (location, security) = match text.rsplit_once('+') {
            Some((location, "insecure")) => (location, Some(true)),
            Some((location, "secure")) => (location, Some(false)),
            Some(_) => return Err(format!("{text:?}: only +insecure or +secure may follow")),
            None => (text, None),
        };
        let (host, prefix) = location.split_once('/').unwrap_or((location, ""));
        if !HOST.is_match(host) {
            return Err(format!("{text:?} does not start with a registry host"));
        }
        if !prefix.is_empty() && !PATH_PREFIX.is_match(prefix) {
            return Err(format!("{text:?} has an invalid repository prefix"));
        }
        // As with CUE_REGISTRY, a registry on the local machine defaults to plain HTTP.
        let local = ["localhost", "127.0.0.1", "[::1]"]
            .iter()
            .any(|name| host == *name || host.starts_with(&format!("{name}:")));
        Ok(Self {
            host: host.to_owned(),
            prefix: prefix.to_owned(),
            insecure: security.unwrap_or(local),
        })
    }
}

impl Source {
    /// The repository a module is in at this source.
    pub fn repository(&self, module_path: &str) -> String {
        if self.prefix.is_empty() {
            module_path.to_owned()
        } else {
            format!("{}/{module_path}", self.prefix)
        }
    }
}

impl Deployment {
    /// The source a module path is copied from: the one whose prefix matches the most whole
    /// path elements.
    pub fn source_for(&self, module_path: &str) -> Option<&Source> {
        self.sources
            .iter()
            .filter(|(prefix, _)| {
                module_path == prefix.as_str()
                    || module_path
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, source)| source)
    }
}

/// Reads the deployment configuration at `path`: a CUE file, or a directory holding the
/// package `forge` (as a module when it has `cue.mod/module.cue`).
pub fn load(path: &Path) -> Result<Deployment, DeploymentError> {
    let read_error = |message: String| DeploymentError::Read {
        path: path.display().to_string(),
        message,
    };
    let metadata = std::fs::metadata(path).map_err(|error| read_error(error.to_string()))?;
    let value = if metadata.is_dir() {
        crate::evaluate_package(path, PACKAGE)
    } else {
        // A single file is evaluated on its own, as `cue export <file>` would.
        let dir = tempfile::tempdir().map_err(|error| read_error(error.to_string()))?;
        let name = path
            .file_name()
            .ok_or_else(|| read_error("no file name".into()))?;
        std::fs::copy(path, dir.path().join(name)).map_err(|e| read_error(e.to_string()))?;
        crate::evaluate_package(dir.path(), PACKAGE)
    };
    parse(value.map_err(DeploymentError::Evaluation)?)
}

/// Reads an exported deployment configuration.
pub fn parse(value: Value) -> Result<Deployment, DeploymentError> {
    let invalid = |message: String| Err(DeploymentError::Invalid(message));
    let Value::Object(mut root) = value else {
        return invalid(format!("the package {PACKAGE} is not a struct"));
    };
    let mut deployment = Deployment::default();
    match root.remove("sources") {
        None => {}
        Some(Value::Object(sources)) => {
            for (prefix, source) in sources {
                if !PATH_PREFIX.is_match(&prefix) {
                    return invalid(format!("sources: {prefix:?} is not a module path prefix"));
                }
                let Value::String(source) = source else {
                    return invalid(format!("sources.{prefix:?} is {source}, not a string"));
                };
                let source = source
                    .parse()
                    .map_err(|e| DeploymentError::Invalid(format!("sources.{prefix:?}: {e}")))?;
                deployment.sources.insert(prefix, source);
            }
        }
        Some(other) => return invalid(format!("sources is {other}, not a struct")),
    }
    match root.remove("modules") {
        None => {}
        Some(Value::Array(modules)) => {
            for module in modules {
                let Value::String(module) = module else {
                    return invalid(format!("modules: {module} is not a string"));
                };
                let module: ModuleVersion = module
                    .parse()
                    .map_err(|e| DeploymentError::Invalid(format!("modules: {e}")))?;
                if deployment.source_for(&module.path).is_none() {
                    return invalid(format!("modules: no source serves {module}"));
                }
                if deployment.modules.contains(&module) {
                    return invalid(format!("modules: {module} is listed twice"));
                }
                deployment.modules.push(module);
            }
        }
        Some(other) => return invalid(format!("modules is {other}, not a list")),
    }
    if let Some(key) = root.keys().next() {
        return invalid(format!(
            "{key:?} is not a setting: the package {PACKAGE} has modules and sources"
        ));
    }
    Ok(deployment)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).expect("write");
    }

    #[test]
    fn loads_a_directory_and_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "forge.cue",
            r#"package forge

let ext = "github.com/rawkode/tartan-ext"

sources: {
	"github.com/rawkode": "ghcr.io/rawkode/cue"
	"tartan.dev":         "localhost:5000"
	(ext):               "registry.example.com:8443/mirror+insecure"
}
modules: [
	"tartan.dev/types@v0.1.0",
	"\(ext)/issues@v0.6.0",
	"github.com/rawkode/other@v1.2.3-rc.1",
]
"#,
        );
        write(dir.path(), "notes.txt", "not CUE\n");
        let deployment = load(dir.path()).expect("valid");
        assert_eq!(
            deployment
                .modules
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "tartan.dev/types@v0.1.0",
                "github.com/rawkode/tartan-ext/issues@v0.6.0",
                "github.com/rawkode/other@v1.2.3-rc.1",
            ]
        );
        let issues = deployment
            .source_for("github.com/rawkode/tartan-ext/issues")
            .expect("a source");
        assert_eq!(
            issues,
            &Source {
                host: "registry.example.com:8443".into(),
                prefix: "mirror".into(),
                insecure: true,
            }
        );
        assert_eq!(
            issues.repository("github.com/rawkode/tartan-ext/issues"),
            "mirror/github.com/rawkode/tartan-ext/issues"
        );
        let other = deployment
            .source_for("github.com/rawkode/other")
            .expect("a source");
        assert_eq!(
            (other.host.as_str(), other.prefix.as_str(), other.insecure),
            ("ghcr.io", "rawkode/cue", false)
        );
        let types = deployment.source_for("tartan.dev/types").expect("a source");
        assert!(types.insecure, "localhost defaults to plain HTTP");
        assert_eq!(deployment.modules[1].major(), "v0");

        let file = load(&dir.path().join("forge.cue")).expect("valid");
        assert_eq!(file, deployment);
    }

    #[test]
    fn an_empty_configuration_lists_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "forge.cue", "package forge\n");
        assert_eq!(load(dir.path()).expect("valid"), Deployment::default());
    }

    #[test]
    fn refuses_invalid_configurations() {
        let refused = |value: Value| parse(value).expect_err("refused").to_string();
        let sources = json!({"github.com/rawkode": "ghcr.io/rawkode"});
        assert!(
            refused(json!({"sources": sources, "modules": ["example.com/x@v0.1.0"]}))
                .contains("no source serves example.com/x@v0.1.0")
        );
        // A prefix matches whole path elements only.
        assert!(
            refused(json!({"sources": sources, "modules": ["github.com/rawkodex/y@v0.1.0"]}))
                .contains("no source")
        );
        for unversioned in [
            "github.com/rawkode/x",
            "github.com/rawkode/x@v0",
            "github.com/rawkode/x@0.1.0",
            "github.com/rawkode/x@latest",
        ] {
            assert!(
                refused(json!({"sources": sources, "modules": [unversioned]})).contains("version"),
                "{unversioned}"
            );
        }
        for bad in [
            json!({"sources": sources, "modules": ["rawkode/x@v0.1.0"]}),
            json!({"sources": sources, "modules": ["github.com/Rawkode/x@v0.1.0"]}),
            json!({"sources": sources, "modules": [1]}),
            json!({"sources": sources, "modules": "github.com/rawkode/x@v0.1.0"}),
            json!({"sources": sources, "modules": ["github.com/rawkode/x@v0.1.0", "github.com/rawkode/x@v0.1.0"]}),
            json!({"sources": {"github.com/rawkode": "https://ghcr.io"}}),
            json!({"sources": {"github.com/rawkode": "ghcr.io+maybe"}}),
            json!({"sources": {"github.com/rawkode": 1}}),
            json!({"sources": {"GitHub": "ghcr.io"}}),
            json!({"registry": "ghcr.io"}),
            json!([]),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn refuses_cue_that_does_not_evaluate() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "forge.cue",
            "package forge\nmodules: [1] & [2]\n",
        );
        assert!(matches!(
            load(dir.path()),
            Err(DeploymentError::Evaluation(_))
        ));
        assert!(matches!(
            load(&dir.path().join("missing.cue")),
            Err(DeploymentError::Read { .. })
        ));
    }
}
