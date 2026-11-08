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
//! import "github.com/rawkode/tartan-ext/issues"
//!
//! extensions: "rawkode.issues": issues.#Extension & {
//!     settings: properties: status: {kind: "select", options: ["todo", "done"]}
//! }
//! ```
//!
//! Every root `*.cue` file declaring `package tartan` is part of it, evaluated as `cue`
//! evaluates a package directory: with the repository's `cue.mod/` when there is one, imports of
//! packages elsewhere in the module, and dependencies on other modules resolved through the
//! registry in the process's `CUE_REGISTRY`, which is the forge's own. Nothing is added to the
//! files a repository commits, so `cue export` against the same registry gives the same value.
//!
//! After evaluation, the exported value must have the shape the forge applies: `extensions` maps
//! an extension id to an entry with a semver `version`, optional object `settings`, and
//! optionally `id` repeating the key. Any other key in an entry is refused, which keeps typos
//! from being silently ignored.

pub mod deployment;
mod files;

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
}

/// One extension's entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    /// The version to install (semver, without a leading `v`).
    pub version: String,
    /// The settings, a JSON object.
    pub settings: Value,
}

/// Why a configuration was refused.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The CUE does not evaluate, its value has the wrong shape, or it is too large.
    #[error("{0}")]
    Invalid(String),
    /// The evaluator or the source failed.
    #[error("the configuration could not be evaluated: {0}")]
    Evaluator(String),
}

/// Evaluates `files`, a repository's configuration package and what it imports. A root with no
/// `package tartan` file is the empty configuration, which installs nothing.
pub async fn evaluate(files: ConfigFiles) -> Result<Config, ConfigError> {
    if !files.has_root_package(PACKAGE) {
        return Ok(Config::default());
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
    let value = evaluate_package(dir.path(), PACKAGE).map_err(ConfigError::Invalid)?;
    check(value)
}

/// The module a directory without its own module is evaluated in; see [`evaluate_package`].
const OUTSIDE_MODULE: &str =
    "module: \"tartan.invalid/outside-any-module\"\nlanguage: version: \"v0.16.0\"\n";

/// The subdirectory of [`OUTSIDE_MODULE`] that holds the directory's files.
const OUTSIDE_DIR: &str = "files";

/// Evaluates the CUE package `package` in directory `dir` as `cue export` would, and returns
/// its value.
///
/// A directory holding `cue.mod/module.cue` is evaluated in place, as that module. cuengine
/// evaluates only inside a module, so a directory without one is evaluated as `cue` evaluates a
/// directory outside any module by copying its root `*.cue` files into a subdirectory of an
/// otherwise empty module: that module adds no file to the package, holds no other package to
/// import, and declares no dependencies, so only the standard library can be imported, exactly
/// as without a module. Its language version is the evaluator's own (CUE v0.16.0), which is what
/// `cue` uses without a module.
pub(crate) fn evaluate_package(dir: &Path, package: &str) -> Result<Value, String> {
    if dir.join(MODULE_FILE).is_file() {
        return evaluate_in_module(dir, None, package, ".");
    }
    let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
    let files = outside.path().join(OUTSIDE_DIR);
    std::fs::create_dir_all(outside.path().join("cue.mod")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    std::fs::write(outside.path().join(MODULE_FILE), OUTSIDE_MODULE).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let is_cue = Path::new(&name).extension().is_some_and(|ext| ext == "cue");
        // Metadata follows symbolic links, as config map mounts use them.
        if is_cue && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()) {
            std::fs::copy(entry.path(), files.join(&name)).map_err(|e| e.to_string())?;
        }
    }
    evaluate_in_module(outside.path(), Some(&files), package, OUTSIDE_DIR)
}

fn evaluate_in_module(
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
    let Value::Object(mut root) = value else {
        return invalid("the package tartan is not a struct".into());
    };
    let extensions = match root.remove("extensions") {
        None => return Ok(Config::default()),
        Some(Value::Object(extensions)) => extensions,
        Some(_) => return invalid("extensions is not a struct".into()),
    };
    let mut config = Config::default();
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
            Some(Value::Null) | None => {
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

    fn files(list: &[(&str, &str)]) -> ConfigFiles {
        ConfigFiles::from_iter(list.iter().map(|(p, c)| ((*p).to_owned(), (*c).to_owned())))
    }

    fn one(source: &str) -> ConfigFiles {
        files(&[("tartan.cue", source)])
    }

    const MODULE: &str = "module: \"example.com/acme\"\nlanguage: version: \"v0.14.1\"\n";

    #[tokio::test]
    async fn evaluates_entries_without_a_module() {
        let config = evaluate(one(r#"package tartan

let options = ["todo", "doing", "done"]

#Issues: {id: "rawkode.issues", version: "0.5.0", settings: {...}}

extensions: "rawkode.issues": #Issues & {
	settings: fields: status: {type: "select", "options": options, default: options[0]}
}
extensions: "rawkode.kanban": version: "0.2.0"
"#))
        .await
        .expect("valid");
        let issues = &config.extensions["rawkode.issues"];
        assert_eq!(issues.version, "0.5.0");
        assert_eq!(issues.settings["fields"]["status"]["default"], "todo");
        let kanban = &config.extensions["rawkode.kanban"];
        assert_eq!(kanban.version, "0.2.0");
        assert_eq!(kanban.settings, json!({}));
    }

    #[tokio::test]
    async fn several_files_make_one_package_and_other_packages_are_ignored() {
        let config = evaluate(files(&[
            (
                "issues.cue",
                "package tartan\nextensions: \"rawkode.issues\": version: \"0.5.0\"\n",
            ),
            (
                "kanban.cue",
                "package tartan\nextensions: \"rawkode.kanban\": version: \"0.2.0\"\n",
            ),
            ("env.cue", "package cuenv\nenv: FOO: \"bar\"\n"),
        ]))
        .await
        .expect("valid");
        let ids: Vec<&str> = config.extensions.keys().map(String::as_str).collect();
        assert_eq!(ids, vec!["rawkode.issues", "rawkode.kanban"]);
    }

    #[tokio::test]
    async fn imports_packages_of_the_module() {
        let config = evaluate(files(&[
            ("cue.mod/module.cue", MODULE),
            (
                "tartan.cue",
                "package tartan\n\nimport defs \"example.com/acme/config/fields\"\n\nextensions: \"rawkode.issues\": {version: \"0.5.0\", settings: fields: defs.all}\n",
            ),
            (
                "config/fields/fields.cue",
                "package fields\nall: owner: type: \"text\"\n",
            ),
        ]))
        .await
        .expect("valid");
        assert_eq!(
            config.extensions["rawkode.issues"].settings["fields"]["owner"]["type"],
            "text"
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
            "package tartan\nextensions: \"rawkode.issues\": version: \"latest\"\n",
            "package tartan\nextensions: \"rawkode.issues\": version: \"v1.0.0\"\n",
            "package tartan\nextensions: \"rawkode.issues\": settings: {}\n",
            "package tartan\nextensions: \"rawkode.issues\": {version: \"1.0.0\", colour: \"red\"}\n",
            "package tartan\nextensions: \"rawkode.issues\": {version: \"1.0.0\", settings: [1]}\n",
            "package tartan\nextensions: \"rawkode.issues\": {id: \"rawkode.kanban\", version: \"1.0.0\"}\n",
            "package tartan\nextensions: \"Bad Id\": version: \"1.0.0\"\n",
            "package tartan\nextensions: \"rawkode.issues\": version: \"1.0.0\" & \"2.0.0\"\n",
            "package tartan\nextensions: \"rawkode.issues\": version: string\n",
            "package tartan\nextensions: [1]\n",
            "package tartan\nthis is not cue\n",
        ];
        for source in bad {
            assert!(evaluate(one(source)).await.is_err(), "{source}");
        }
    }

    #[tokio::test]
    async fn adds_no_files_to_the_evaluation() {
        // A module that declares no dependencies cannot import one: nothing is added to it.
        let unresolved = files(&[
            ("cue.mod/module.cue", MODULE),
            (
                "tartan.cue",
                "package tartan\n\nimport \"tartan.test/nowhere/issues\"\n\nextensions: \"rawkode.issues\": issues.#Extension\n",
            ),
        ]);
        assert!(evaluate(unresolved).await.is_err());
        // Without a module, a root file named like the old schema file is evaluated as written.
        let config = evaluate(files(&[
            ("tartan.cue", "package tartan\n"),
            (
                "zz_tartan_schema.cue",
                "package tartan\nextensions: \"rawkode.issues\": version: \"1.0.0\"\n",
            ),
        ]))
        .await
        .expect("valid");
        assert_eq!(config.extensions["rawkode.issues"].version, "1.0.0");
        // Without a module only the standard library can be imported, as with `cue`.
        let stdlib = one(
            "package tartan\n\nimport \"strings\"\n\nextensions: \"rawkode.issues\": version: strings.Join([\"1\", \"2\", \"0\"], \".\")\n",
        );
        let config = evaluate(stdlib).await.expect("valid");
        assert_eq!(config.extensions["rawkode.issues"].version, "1.2.0");
        let elsewhere = one(
            "package tartan\n\nimport \"tartan.invalid/outside-any-module/files\"\n\nx: files\n",
        );
        assert!(evaluate(elsewhere).await.is_err());
    }

    #[test]
    fn checks_the_exported_shape() {
        let config = check(json!({
            "extensions": {"rawkode.issues": {"id": "rawkode.issues", "version": "0.6.0"}},
            "helpers": {"anything": true},
        }))
        .expect("valid");
        assert_eq!(config.extensions["rawkode.issues"].settings, json!({}));
        assert!(check(json!([])).is_err());
        assert!(check(json!({"extensions": {"a": {"version": null}}})).is_err());
        assert!(check(json!({"extensions": {"a": {"version": "1.0.0+build.1"}}})).is_ok());
    }
}
