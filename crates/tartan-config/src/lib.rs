//! CUE configuration, evaluated natively: what `cue export` gives is what the forge applies.
//!
//! - A repository's configuration is the CUE package `tartan` in its root directory. [`load`]
//!   reads what the package needs from a [`ConfigSource`]; [`evaluate`] evaluates exactly those
//!   files and checks the exported value's shape.
//! - The deployment's configuration is the CUE package `forge` named by `TARTAN_CONFIG`; see
//!   [`deployment`].
//!
//! ```cue
//! package tartan
//!
//! import "tartan.rawkode.academy/issues"
//!
//! extensions: "issues.tartan.rawkode.academy": issues.#Extension & {
//!     settings: properties: status: {kind: "select", options: ["todo", "done"]}
//! }
//! ```
//!
//! Every root `*.cue` file declaring `package tartan` is part of it, evaluated as `cue export`
//! evaluates a package directory of a module: the repository's `cue.mod/module.cue` is required,
//! packages elsewhere in the module can be imported, files embedded with `@embed` are read from
//! the repository, and dependencies on other modules resolve through the registry in the
//! process's `CUE_REGISTRY`, which is the forge's own. Nothing is added to the files a repository
//! commits, so `cue export` against the same registry gives the same value. A root `package
//! tartan` without `cue.mod/module.cue` is refused: the evaluator only evaluates inside a module,
//! and the forge supplies none of its own.
//!
//! `cue export` refuses a value that is not concrete, such as `string` or a disjunction without
//! a default. The evaluator exports such a value as `null`, so the forge refuses every `null` in
//! the package's value: an explicit `null` cannot be told apart from a value that is not
//! concrete, and leaving a field out says the same thing.
//!
//! After evaluation, the exported value must have the shape the forge applies: `extensions` maps
//! an extension id to an entry with a semver `version`, optional object `settings`, and
//! optionally `id` repeating the key. Any other key in an entry is refused, which keeps typos
//! from being silently ignored. An optional top-level `signing` is the repository's signing rule
//! ([`signing`]); it is checked whether or not `extensions` is present.

pub mod deployment;
pub mod did;
mod files;
pub mod signing;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::files::{ConfigFiles, ConfigSource, DirEntry, LIMITS, Limits, load};

/// The CUE package a repository's configuration is.
pub const PACKAGE: &str = "tartan";

/// The file that makes a directory a CUE module.
const MODULE_FILE: &str = "cue.mod/module.cue";

/// An evaluated configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Each extension the configuration installs, by id.
    #[serde(default)]
    pub extensions: BTreeMap<String, ExtensionEntry>,
    /// The signing rule, when the configuration sets `signing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<signing::SigningRule>,
}

/// One extension's entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    /// The version to install (semver, without a leading `v`).
    pub version: String,
    /// The settings, a JSON object.
    pub settings: Value,
}

/// Why a configuration was not applied.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The CUE does not evaluate, its value has the wrong shape, or it is too large. Evaluating
    /// the same commit again gives the same answer, so the configuration is refused.
    #[error("{0}")]
    Invalid(String),
    /// The evaluator, the repository's source or the registry failed. Evaluating the same commit
    /// later may succeed.
    #[error("the configuration could not be evaluated: {0}")]
    Evaluator(String),
}

impl ConfigError {
    /// Whether evaluating the same commit again may give another answer.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Evaluator(_))
    }
}

/// Evaluates `files`, a repository's configuration package and what it imports. A root with no
/// `package tartan` file is the empty configuration, which installs nothing.
pub async fn evaluate(files: ConfigFiles) -> Result<Config, ConfigError> {
    if !files.has_root_package(PACKAGE) {
        return Ok(Config::default());
    }
    if !files.contains(MODULE_FILE) {
        return Err(ConfigError::Invalid(format!(
            "the package {PACKAGE} is not in a CUE module: commit {MODULE_FILE} (`cue mod init`) \
             beside it, as `cue export` of the package with its imports needs"
        )));
    }
    tokio::task::spawn_blocking(move || evaluate_blocking(&files))
        .await
        .map_err(|error| ConfigError::Evaluator(error.to_string()))?
}

fn evaluate_blocking(files: &ConfigFiles) -> Result<Config, ConfigError> {
    let dir = tempfile::tempdir().map_err(|error| ConfigError::Evaluator(error.to_string()))?;
    for (path, contents) in files.iter() {
        write(dir.path(), path, contents)?;
    }
    let value = evaluate_module_package(dir.path(), None, PACKAGE, ".").map_err(classify)?;
    check(value)
}

/// Fragments of the evaluator's messages that say the registry could not be reached or failed,
/// rather than that it answered: a module it does not hold is a 404, which is final.
const TRANSIENT_REGISTRY_FAILURES: &[&str] = &[
    "connection refused",
    "connection reset",
    "i/o timeout",
    "context deadline exceeded",
    "TLS handshake timeout",
    "500 Internal Server Error",
    "502 Bad Gateway",
    "503 Service Unavailable",
    "504 Gateway Timeout",
    "429 Too Many Requests",
];

/// Sorts an evaluation failure: one the registry caused by failing may not recur, any other
/// will.
fn classify(message: String) -> ConfigError {
    if TRANSIENT_REGISTRY_FAILURES
        .iter()
        .any(|fragment| message.contains(fragment))
    {
        ConfigError::Evaluator(message)
    } else {
        ConfigError::Invalid(message)
    }
}

/// Evaluates the CUE package `package` of the module rooted at `root` as `cue export` would, and
/// returns the value of the instance `instance` (its directory relative to `root`); `target` is
/// that directory when it is not the root.
pub(crate) fn evaluate_module_package(
    root: &Path,
    target: Option<&Path>,
    package: &str,
    instance: &str,
) -> Result<Value, String> {
    let options = cuengine::ModuleEvalOptions {
        package_name: Some(package.to_owned()),
        target_dir: target.map(|dir| dir.display().to_string()),
        ..cuengine::ModuleEvalOptions::default()
    };
    let result =
        cuengine::evaluate_module(root, package, Some(&options)).map_err(|e| e.to_string())?;
    result
        .instances
        .get(instance)
        .cloned()
        .ok_or_else(|| format!("the package {package} did not evaluate"))
}

/// The path of the first `null` in `value`, written as CUE writes a path (`a.b[0]."c.d"`), if
/// there is one. The evaluator exports a value that is not concrete as `null`.
pub(crate) fn first_null(value: &Value) -> Option<String> {
    fn label(name: &str) -> String {
        let plain = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
        if plain {
            name.to_owned()
        } else {
            serde_json::to_string(name).unwrap_or_else(|_| name.to_owned())
        }
    }
    fn walk(value: &Value, path: &mut String) -> bool {
        match value {
            Value::Null => true,
            Value::Array(items) => items.iter().enumerate().any(|(index, item)| {
                let length = path.len();
                path.push_str(&format!("[{index}]"));
                let found = walk(item, path);
                if !found {
                    path.truncate(length);
                }
                found
            }),
            Value::Object(fields) => fields.iter().any(|(name, field)| {
                let length = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(&label(name));
                let found = walk(field, path);
                if !found {
                    path.truncate(length);
                }
                found
            }),
            _ => false,
        }
    }
    let mut path = String::new();
    walk(value, &mut path).then_some(path)
}

static EXTENSION_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._-]{0,63}$").expect("valid regex"));
static SEMVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$",
    )
    .expect("valid regex")
});

/// Checks an exported configuration's shape and reads it.
pub fn check(value: Value) -> Result<Config, ConfigError> {
    let invalid = |message: String| Err(ConfigError::Invalid(message));
    if let Some(path) = first_null(&value) {
        return invalid(format!(
            "{} is not concrete or is null: give it a value, or leave the field out",
            if path.is_empty() {
                "the package tartan"
            } else {
                path.as_str()
            }
        ));
    }
    let Value::Object(mut root) = value else {
        return invalid("the package tartan is not a struct".into());
    };
    let mut config = Config::default();
    if let Some(signing) = root.remove("signing") {
        config.signing = Some(signing::check(signing)?);
    }
    let extensions = match root.remove("extensions") {
        None => return Ok(config),
        Some(Value::Object(extensions)) => extensions,
        Some(_) => return invalid("extensions is not a struct".into()),
    };
    for (id, entry) in extensions {
        if !EXTENSION_ID.is_match(&id) {
            return invalid(format!("{id:?} is not an extension id"));
        }
        let Value::Object(mut entry) = entry else {
            return invalid(format!("extensions.{id:?} is not a struct"));
        };
        match entry.remove("id") {
            None => {}
            Some(Value::String(named)) if named == id => {}
            Some(other) => {
                return invalid(format!("extensions.{id:?} has id {other}, not its own key"));
            }
        }
        let version = match entry.remove("version") {
            Some(Value::String(version)) if SEMVER.is_match(&version) => version,
            None => {
                return invalid(format!("extensions.{id:?} has no version"));
            }
            Some(other) => {
                return invalid(format!(
                    "extensions.{id:?} has version {other}, not a semver version such as \"1.2.0\""
                ));
            }
        };
        let settings = match entry.remove("settings") {
            None => Value::Object(serde_json::Map::new()),
            Some(settings @ Value::Object(_)) => settings,
            Some(other) => {
                return invalid(format!(
                    "extensions.{id:?}.settings is {other}, not a struct"
                ));
            }
        };
        if let Some(key) = entry.keys().next() {
            return invalid(format!(
                "extensions.{id:?} has {key:?}: an entry has only id, version and settings"
            ));
        }
        config
            .extensions
            .insert(id, ExtensionEntry { version, settings });
    }
    Ok(config)
}

fn write(dir: &Path, name: &str, contents: &str) -> Result<(), ConfigError> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::Evaluator(e.to_string()))?;
    }
    std::fs::write(&path, contents).map_err(|error| ConfigError::Evaluator(error.to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const MODULE: &str = "module: \"example.com/acme\"\nlanguage: version: \"v0.14.1\"\n";

    /// A repository: a module holding `list`.
    fn files(list: &[(&str, &str)]) -> ConfigFiles {
        ConfigFiles::from_iter(
            std::iter::once(&("cue.mod/module.cue", MODULE))
                .chain(list)
                .map(|(p, c)| ((*p).to_owned(), (*c).to_owned())),
        )
    }

    fn one(source: &str) -> ConfigFiles {
        files(&[("tartan.cue", source)])
    }

    #[tokio::test]
    async fn evaluates_entries() {
        let config = evaluate(one(r#"package tartan

let options = ["todo", "doing", "done"]

#Issues: {id: "issues.tartan.rawkode.academy", version: "0.5.0", settings: {...}}

extensions: "issues.tartan.rawkode.academy": #Issues & {
	settings: fields: status: {type: "select", "options": options, default: options[0]}
}
extensions: "kanban.tartan.rawkode.academy": version: "0.2.0"
"#))
        .await
        .expect("valid");
        let issues = &config.extensions["issues.tartan.rawkode.academy"];
        assert_eq!(issues.version, "0.5.0");
        assert_eq!(issues.settings["fields"]["status"]["default"], "todo");
        let kanban = &config.extensions["kanban.tartan.rawkode.academy"];
        assert_eq!(kanban.version, "0.2.0");
        assert_eq!(kanban.settings, json!({}));
    }

    #[tokio::test]
    async fn several_files_make_one_package_and_other_packages_are_ignored() {
        let config = evaluate(files(&[
            (
                "issues.cue",
                "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: \"0.5.0\"\n",
            ),
            (
                "kanban.cue",
                "package tartan\nextensions: \"kanban.tartan.rawkode.academy\": version: \"0.2.0\"\n",
            ),
            ("env.cue", "package cuenv\nenv: FOO: \"bar\"\n"),
        ]))
        .await
        .expect("valid");
        let ids: Vec<&str> = config.extensions.keys().map(String::as_str).collect();
        assert_eq!(
            ids,
            vec![
                "issues.tartan.rawkode.academy",
                "kanban.tartan.rawkode.academy"
            ]
        );
    }

    #[tokio::test]
    async fn imports_packages_of_the_module() {
        let config = evaluate(files(&[
            (
                "tartan.cue",
                "package tartan\n\nimport defs \"example.com/acme/config/fields\"\n\nextensions: \"issues.tartan.rawkode.academy\": {version: \"0.5.0\", settings: fields: defs.all}\n",
            ),
            (
                "config/fields/fields.cue",
                "package fields\nall: owner: type: \"text\"\n",
            ),
        ]))
        .await
        .expect("valid");
        assert_eq!(
            config.extensions["issues.tartan.rawkode.academy"].settings["fields"]["owner"]["type"],
            "text"
        );
    }

    #[tokio::test]
    async fn embeds_files_of_the_module() {
        let config = evaluate(files(&[
            (
                "tartan.cue",
                "@extern(embed)\n\npackage tartan\n\nextensions: \"issues.tartan.rawkode.academy\": {version: \"0.6.0\", settings: properties: _ @embed(file=\"config/properties.json\")}\n",
            ),
            ("config/properties.json", r#"{"due": {"kind": "date"}}"#),
        ]))
        .await
        .expect("valid");
        assert_eq!(
            config.extensions["issues.tartan.rawkode.academy"].settings["properties"]["due"]["kind"],
            "date"
        );
    }

    #[tokio::test]
    async fn no_tartan_package_installs_nothing() {
        assert_eq!(
            evaluate(ConfigFiles::default()).await.expect("valid"),
            Config::default()
        );
        let other = files(&[("env.cue", "package cuenv\nenv: {}\n")]);
        assert_eq!(evaluate(other).await.expect("valid"), Config::default());
        let empty = one("package tartan\n");
        assert_eq!(evaluate(empty).await.expect("valid"), Config::default());
    }

    #[tokio::test]
    async fn refuses_what_does_not_fit() {
        let bad = [
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: \"latest\"\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: \"v1.0.0\"\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": settings: {}\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": {version: \"1.0.0\", colour: \"red\"}\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": {version: \"1.0.0\", settings: [1]}\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": {id: \"kanban.tartan.rawkode.academy\", version: \"1.0.0\"}\n",
            "package tartan\nextensions: \"Bad Id\": version: \"1.0.0\"\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: \"1.0.0\" & \"2.0.0\"\n",
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: string\n",
            "package tartan\nextensions: [1]\n",
            "package tartan\nthis is not cue\n",
        ];
        for source in bad {
            let error = evaluate(one(source)).await.expect_err(source);
            assert!(!error.is_transient(), "{source}: {error}");
        }
    }

    #[tokio::test]
    async fn refuses_values_that_are_not_concrete_as_cue_export_does() {
        let entry = |settings: &str| {
            format!(
                "package tartan\nextensions: \"kanban.tartan.rawkode.academy\": {{version: \"0.3.0\", settings: {settings}}}\n"
            )
        };
        for (settings, path) in [
            ("{x: string}", "settings.x"),
            ("{x: >5}", "settings.x"),
            ("{x!: string}", "settings.x"),
            ("{x: {b: \"x\"} | {b: \"y\"}}", "settings.x"),
            ("{x: [string, ...string]}", "settings.x[0]"),
            (
                "{boards: t: {name: \"T\", columns: _}}",
                "settings.boards.t.columns",
            ),
            ("{\"a b\": null}", "settings.\"a b\""),
        ] {
            let error = evaluate(one(&entry(settings)))
                .await
                .expect_err(settings)
                .to_string();
            assert!(
                error.contains(&format!(
                    "extensions.\"kanban.tartan.rawkode.academy\".{path} is not concrete"
                )),
                "{settings}: {error}"
            );
        }
        // Outside extensions, too: `cue export` of the package fails.
        assert!(
            evaluate(one("package tartan\nnote: string\n"))
                .await
                .is_err()
        );
        // Defaults, open lists and optional fields are concrete.
        let config = evaluate(one(&entry("{x: int | *3, y: [...string], z?: string}")))
            .await
            .expect("valid");
        assert_eq!(
            config.extensions["kanban.tartan.rawkode.academy"].settings,
            json!({"x": 3, "y": []})
        );
    }

    #[tokio::test]
    async fn adds_no_files_to_the_evaluation() {
        // A module that declares no dependencies cannot import one: nothing is added to it.
        let unresolved = one(
            "package tartan\n\nimport \"tartan.test/nowhere/issues\"\n\nextensions: \"issues.tartan.rawkode.academy\": issues.#Extension\n",
        );
        assert!(evaluate(unresolved).await.is_err());
        // A root file named like an injected schema file is evaluated as written.
        let config = evaluate(files(&[
            ("tartan.cue", "package tartan\n"),
            (
                "zz_tartan_schema.cue",
                "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: \"1.0.0\"\n",
            ),
        ]))
        .await
        .expect("valid");
        assert_eq!(
            config.extensions["issues.tartan.rawkode.academy"].version,
            "1.0.0"
        );
        // Without a module the package is refused: the forge supplies no module of its own.
        let outside = ConfigFiles::from_iter([(
            "tartan.cue".to_owned(),
            "package tartan\nextensions: \"issues.tartan.rawkode.academy\": version: \"1.0.0\"\n"
                .to_owned(),
        )]);
        let error = evaluate(outside).await.expect_err("no module").to_string();
        assert!(error.contains("cue.mod/module.cue"), "{error}");
    }

    #[test]
    fn registry_failures_are_transient_and_answers_are_not() {
        assert!(
            classify("dial tcp 127.0.0.1:8080: connect: connection refused".into()).is_transient()
        );
        assert!(classify("503 Service Unavailable: UNAVAILABLE".into()).is_transient());
        assert!(
            !classify("404 Not Found: module example.com/x@v0.1.0 not found".into()).is_transient()
        );
        assert!(!classify("conflicting values 1 and 2".into()).is_transient());
    }

    #[test]
    fn checks_the_exported_shape() {
        let config = check(json!({
            "extensions": {"issues.tartan.rawkode.academy": {"id": "issues.tartan.rawkode.academy", "version": "0.6.0"}},
            "helpers": {"anything": true},
        }))
        .expect("valid");
        assert_eq!(
            config.extensions["issues.tartan.rawkode.academy"].settings,
            json!({})
        );
        assert!(check(json!([])).is_err());
        assert!(check(json!({"extensions": {"a": {"version": null}}})).is_err());
        assert!(check(json!({"helpers": [1, null]})).is_err());
        assert!(check(json!({"extensions": {"a": {"version": "1.0.0+build.1"}}})).is_ok());
    }

    #[test]
    fn checks_signing_without_extensions() {
        let rule = json!({"require": [{"refs": ["refs/heads/main"], "signers": ["did:plc:abcdefghijklmnopqrstuvwx"]}]});
        let config = check(json!({"signing": rule})).expect("valid");
        assert!(config.extensions.is_empty());
        let signing = config.signing.expect("a rule");
        assert!(signing.coverage("refs/heads/main").is_covered());
        let typo = check(json!({"signing": {"requires": []}})).expect_err("a typo");
        assert!(typo.to_string().contains("signing.requires"), "{typo}");
        let with_extensions = check(json!({
            "signing": {"require": [{"refs": ["refs/heads/main"]}]},
            "extensions": {"issues.tartan.rawkode.academy": {"version": "0.6.0"}},
        }))
        .expect("valid");
        assert!(with_extensions.signing.is_some());
        assert_eq!(with_extensions.extensions.len(), 1);
        assert!(
            check(json!({"extensions": {}}))
                .expect("valid")
                .signing
                .is_none()
        );
    }

    #[tokio::test]
    async fn evaluates_a_root_with_signing_but_no_extensions() {
        let config = evaluate(one(
            "package tartan\nsigning: require: [{refs: [\"refs/heads/main\", \"refs/tags/**\"], signers: [\"did:plc:abcdefghijklmnopqrstuvwx\"]}]\n",
        ))
        .await
        .expect("valid");
        assert!(config.extensions.is_empty());
        assert!(
            config
                .signing
                .expect("a rule")
                .coverage("refs/tags/v1")
                .is_covered()
        );
        let typo = evaluate(one("package tartan\nsigning: requires: []\n"))
            .await
            .expect_err("a typo");
        assert!(typo.to_string().contains("signing.requires"), "{typo}");
    }
}
