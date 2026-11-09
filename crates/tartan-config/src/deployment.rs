//! The deployment's configuration: the CUE package `forge` in the file or directory named by
//! `TARTAN_CONFIG`, usually mounted from the deployment's config map.
//!
//! ```cue
//! package forge
//!
//! sources: "tartan.rawkode.academy": "ghcr.io/rawkode/cue"
//! modules: [
//!     "tartan.rawkode.academy/types@v0.1.0",
//!     "tartan.rawkode.academy/issues@v0.6.0",
//! ]
//! owners: ["did:plc:abcdefghijklmnopqrstuvwx"]
//! readers: "owners"
//! ```
//!
//! `modules` lists the CUE module versions the forge makes available, each a fully qualified
//! `path@vX.Y.Z`. `sources` maps module path prefixes to the OCI registries the forge copies
//! them from, written as `CUE_REGISTRY` writes a registry: `host[:port][/repository-prefix]`,
//! optionally with `+insecure` (plain HTTP) or `+secure`. A module comes from the source with
//! the longest prefix that matches whole path elements of its path.
//!
//! `owners` lists the DIDs of the forge's administrators: at least one, each a distinct
//! `did:plc` or hostname-only `did:web` DID ([`Did`]). `readers` says who else may read:
//! `"owners"` (the default when it is absent: nobody else), `"signed_in"` (any ATProto account
//! that signs in) or a list of distinct DIDs. Both change only when the forge restarts with a new
//! configuration.
//!
//! The package is evaluated natively, with no registry: it is the operator's file, not a
//! repository's, and imports nothing but CUE's standard library. CUE's evaluator in the forge
//! evaluates only inside a module, so a file or a directory without `cue.mod/module.cue` is
//! evaluated in an otherwise empty module made for it (see [`OUTSIDE_MODULE`]); unlike a
//! repository's configuration, the deployment's is never resolved through a registry, so that
//! module changes nothing `cue export` of the file gives. A configuration that does not evaluate,
//! has a key other than `modules`, `sources`, `owners` and `readers`, lists a module that is not
//! fully qualified or names no source for it, lists no owner, or lists an invalid or repeated DID
//! is refused, and the forge does not start.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

pub use crate::did::Did;

/// The CUE package a deployment's configuration is.
pub const PACKAGE: &str = "forge";

/// A deployment's configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deployment {
    /// The module versions the forge makes available, in the order listed.
    pub modules: Vec<ModuleVersion>,
    /// The registries modules are copied from, by module path prefix.
    pub sources: BTreeMap<String, Source>,
    /// The forge's administrators, in the order listed; [`parse`] refuses an empty list.
    pub owners: Vec<Did>,
    /// Who other than the owners may read.
    pub readers: Readers,
}

/// Who other than the forge's owners may read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Readers {
    /// Nobody else: other people who sign in see that the forge is private (`"owners"`).
    #[default]
    Owners,
    /// Any ATProto account that signs in (`"signed_in"`).
    SignedIn,
    /// These DIDs, in the order listed.
    Dids(Vec<Did>),
}

impl Readers {
    /// Whether `did` may read under this setting, owners aside.
    pub fn admits(&self, did: &str) -> bool {
        match self {
            Self::Owners => false,
            Self::SignedIn => true,
            Self::Dids(dids) => dids.iter().any(|reader| reader.as_str() == did),
        }
    }
}

/// One version of a CUE module, such as `tartan.rawkode.academy/issues@v0.6.0`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleVersion {
    /// The module path, without a major version suffix: `tartan.rawkode.academy/issues`.
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
    /// Whether `did` is one of the forge's owners.
    pub fn is_owner(&self, did: &str) -> bool {
        self.owners.iter().any(|owner| owner.as_str() == did)
    }

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
        evaluate_directory(path)
    } else {
        // A single file is evaluated on its own, as `cue export <file>` would.
        let dir = tempfile::tempdir().map_err(|error| read_error(error.to_string()))?;
        let name = path
            .file_name()
            .ok_or_else(|| read_error("no file name".into()))?;
        std::fs::copy(path, dir.path().join(name)).map_err(|e| read_error(e.to_string()))?;
        evaluate_directory(dir.path())
    };
    parse(value.map_err(DeploymentError::Evaluation)?)
}

/// The module a deployment configuration without its own module is evaluated in: it declares
/// no dependencies and holds no package to import, so only the standard library can be
/// imported, as outside any module. Its language version is the evaluator's (CUE v0.16.0).
const OUTSIDE_MODULE: &str =
    "module: \"tartan.invalid/outside-any-module\"\nlanguage: version: \"v0.16.0\"\n";

/// The subdirectory of [`OUTSIDE_MODULE`] that holds the configuration's files.
const OUTSIDE_DIR: &str = "files";

/// Evaluates the package `forge` in `dir`: in place when `dir` is a module, otherwise its root
/// `*.cue` files in [`OUTSIDE_MODULE`].
fn evaluate_directory(dir: &Path) -> Result<Value, String> {
    if dir.join("cue.mod/module.cue").is_file() {
        return crate::evaluate_module_package(dir, None, PACKAGE, ".");
    }
    let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
    let files = outside.path().join(OUTSIDE_DIR);
    std::fs::create_dir_all(outside.path().join("cue.mod")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    std::fs::write(outside.path().join("cue.mod/module.cue"), OUTSIDE_MODULE)
        .map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let is_cue = Path::new(&name).extension().is_some_and(|ext| ext == "cue");
        // Metadata follows symbolic links, as config map mounts use them.
        if is_cue && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()) {
            std::fs::copy(entry.path(), files.join(&name)).map_err(|e| e.to_string())?;
        }
    }
    crate::evaluate_module_package(outside.path(), Some(&files), PACKAGE, OUTSIDE_DIR)
}

/// Reads an exported deployment configuration.
pub fn parse(value: Value) -> Result<Deployment, DeploymentError> {
    let invalid = |message: String| Err(DeploymentError::Invalid(message));
    if let Some(path) = crate::first_null(&value) {
        return invalid(format!("{path} is not concrete or is null"));
    }
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
    deployment.owners = match root.remove("owners") {
        None => return invalid(no_owners()),
        Some(Value::Array(owners)) if owners.is_empty() => return invalid(no_owners()),
        Some(Value::Array(owners)) => dids("owners", owners)?,
        Some(other) => return invalid(format!("owners is {other}, not a list of DIDs")),
    };
    deployment.readers = match root.remove("readers") {
        None => Readers::Owners,
        Some(Value::String(mode)) if mode == "owners" => Readers::Owners,
        Some(Value::String(mode)) if mode == "signed_in" => Readers::SignedIn,
        Some(Value::Array(readers)) => Readers::Dids(dids("readers", readers)?),
        Some(other) => {
            return invalid(format!(
                "readers is {other}: it is \"owners\" (the default), \"signed_in\" or a list of DIDs"
            ));
        }
    };
    if let Some(key) = root.keys().next() {
        return invalid(format!(
            "{key:?} is not a setting: the package {PACKAGE} has modules, sources, owners and readers"
        ));
    }
    Ok(deployment)
}

fn no_owners() -> String {
    "owners must list at least one DID: the forge's administrators (find a DID from a handle with `tt identity resolve <handle>`)".into()
}

/// Reads a list of distinct DIDs, naming `setting` and the entry in any error.
fn dids(setting: &str, values: Vec<Value>) -> Result<Vec<Did>, DeploymentError> {
    let mut dids: Vec<Did> = Vec::with_capacity(values.len());
    for (index, value) in values.into_iter().enumerate() {
        let Value::String(text) = value else {
            return Err(DeploymentError::Invalid(format!(
                "{setting}[{index}] is {value}, not a DID"
            )));
        };
        let did: Did = text
            .parse()
            .map_err(|e| DeploymentError::Invalid(format!("{setting}[{index}]: {e}")))?;
        if dids.contains(&did) {
            return Err(DeploymentError::Invalid(format!(
                "{setting}[{index}]: {did} is listed twice"
            )));
        }
        dids.push(did);
    }
    Ok(dids)
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

let issues = "tartan.rawkode.academy/issues"

sources: {
	"github.com/rawkode":     "ghcr.io/rawkode/cue"
	"tartan.rawkode.academy": "localhost:5000"
	(issues):                 "registry.example.com:8443/mirror+insecure"
}
modules: [
	"tartan.rawkode.academy/types@v0.1.0",
	"\(issues)@v0.6.0",
	"github.com/rawkode/other@v1.2.3-rc.1",
]
owners: ["did:web:rawkode.dev", "did:plc:abcdefghijklmnopqrstuvwx"]
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
                "tartan.rawkode.academy/types@v0.1.0",
                "tartan.rawkode.academy/issues@v0.6.0",
                "github.com/rawkode/other@v1.2.3-rc.1",
            ]
        );
        let issues = deployment
            .source_for("tartan.rawkode.academy/issues")
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
            issues.repository("tartan.rawkode.academy/issues"),
            "mirror/tartan.rawkode.academy/issues"
        );
        let other = deployment
            .source_for("github.com/rawkode/other")
            .expect("a source");
        assert_eq!(
            (other.host.as_str(), other.prefix.as_str(), other.insecure),
            ("ghcr.io", "rawkode/cue", false)
        );
        let types = deployment
            .source_for("tartan.rawkode.academy/types")
            .expect("a source");
        assert!(types.insecure, "localhost defaults to plain HTTP");
        assert_eq!(deployment.modules[1].major(), "v0");
        assert_eq!(
            deployment
                .owners
                .iter()
                .map(Did::as_str)
                .collect::<Vec<_>>(),
            vec!["did:web:rawkode.dev", "did:plc:abcdefghijklmnopqrstuvwx"]
        );
        assert!(deployment.is_owner("did:web:rawkode.dev"));
        assert!(!deployment.is_owner("did:web:example.com"));
        assert_eq!(deployment.readers, Readers::Owners, "the default");

        let file = load(&dir.path().join("forge.cue")).expect("valid");
        assert_eq!(file, deployment);
    }

    const OWNER: &str = "did:plc:abcdefghijklmnopqrstuvwx";

    #[test]
    fn owners_alone_make_a_configuration() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "forge.cue",
            &format!("package forge\nowners: [{OWNER:?}]\n"),
        );
        let deployment = load(dir.path()).expect("valid");
        assert!(deployment.modules.is_empty() && deployment.sources.is_empty());
        assert_eq!(
            deployment.owners,
            vec![OWNER.parse::<Did>().expect("a DID")]
        );
        assert_eq!(deployment.readers, Readers::Owners);
    }

    #[test]
    fn refuses_missing_and_invalid_owners() {
        let refused = |value: Value| parse(value).expect_err("refused").to_string();
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "forge.cue", "package forge\n");
        assert!(
            load(dir.path())
                .expect_err("no owners")
                .to_string()
                .contains("owners must list at least one DID")
        );
        let sources = json!({"github.com/rawkode": "ghcr.io/rawkode"});
        assert!(
            refused(json!({"sources": sources, "modules": []}))
                .contains("owners must list at least one DID")
        );
        assert!(refused(json!({"owners": []})).contains("owners must list at least one DID"));
        assert!(refused(json!({"owners": OWNER})).contains("not a list of DIDs"));
        assert!(refused(json!({"owners": [1]})).contains("owners[0] is 1, not a DID"));
        let handle = refused(json!({"owners": ["alice.bsky.social"]}));
        assert!(
            handle.contains("owners[0]") && handle.contains("\"alice.bsky.social\" is not a DID"),
            "{handle}"
        );
        let path = refused(json!({"owners": [OWNER, "did:web:example.com:alice"]}));
        assert!(
            path.contains("owners[1]") && path.contains("only hostname-level did:web"),
            "{path}"
        );
        let twice = refused(json!({"owners": [OWNER, "did:web:rawkode.dev", OWNER]}));
        assert!(
            twice.contains(&format!("owners[2]: {OWNER} is listed twice")),
            "{twice}"
        );
        assert!(refused(json!({"owners": ["did:web:example.com%3A443"]})).contains("port"));
        // A localhost DID is syntactically valid; the forge refuses it outside dev-identity.
        let local = parse(json!({"owners": ["did:web:localhost%3A9400"]})).expect("valid");
        assert!(local.owners[0].is_localhost());
    }

    #[test]
    fn reads_who_may_read() {
        let readers = |value: Value| {
            parse(json!({"owners": [OWNER], "readers": value}))
                .map(|deployment| deployment.readers)
                .map_err(|error| error.to_string())
        };
        assert_eq!(readers(json!("owners")), Ok(Readers::Owners));
        assert_eq!(readers(json!("signed_in")), Ok(Readers::SignedIn));
        let listed = readers(json!([
            "did:web:rawkode.dev",
            "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb"
        ]))
        .expect("valid");
        assert_eq!(
            listed,
            Readers::Dids(vec![
                "did:web:rawkode.dev".parse().expect("a DID"),
                "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb".parse().expect("a DID"),
            ])
        );
        assert!(listed.admits("did:web:rawkode.dev"));
        assert!(!listed.admits("did:web:example.com"));
        assert!(Readers::SignedIn.admits("did:web:example.com"));
        assert!(!Readers::Owners.admits(OWNER));

        let everyone = readers(json!("everyone")).expect_err("refused");
        assert!(
            everyone.contains("readers is \"everyone\"")
                && everyone.contains("\"owners\" (the default), \"signed_in\" or a list of DIDs"),
            "{everyone}"
        );
        assert!(readers(json!(true)).is_err());
        assert!(readers(json!({"did": OWNER})).is_err());
        let twice =
            readers(json!(["did:web:rawkode.dev", "did:web:rawkode.dev"])).expect_err("refused");
        assert!(
            twice.contains("readers[1]: did:web:rawkode.dev is listed twice"),
            "{twice}"
        );
        let invalid = readers(json!(["did:web:rawkode.dev", "bob.test"])).expect_err("refused");
        assert!(
            invalid.contains("readers[1]") && invalid.contains("not a DID"),
            "{invalid}"
        );
        assert!(
            readers(json!([2]))
                .expect_err("refused")
                .contains("readers[0] is 2, not a DID")
        );
    }

    #[test]
    fn refuses_invalid_configurations() {
        let refused = |value: Value| parse(value).expect_err("refused").to_string();
        let unknown = refused(json!({"owners": [OWNER], "registry": "ghcr.io"}));
        assert!(
            unknown.contains("\"registry\" is not a setting")
                && unknown.contains("modules, sources, owners and readers"),
            "{unknown}"
        );
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
        write(
            dir.path(),
            "forge.cue",
            "package forge\nmodules: [string]\n",
        );
        assert!(
            load(dir.path())
                .expect_err("not concrete")
                .to_string()
                .contains("modules[0] is not concrete")
        );
        assert!(matches!(
            load(&dir.path().join("missing.cue")),
            Err(DeploymentError::Read { .. })
        ));
    }
}
