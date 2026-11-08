//! A repository's configuration: the CUE package `tartan` in its root directory.
//!
//! ```cue
//! package tartan
//!
//! extensions: "rawkode.issues": {
//!     version: "0.5.0"
//!     settings: fields: status: {type: "select", options: ["todo", "done"], default: "todo"}
//! }
//!
//! // No version: settings for the org's installation of an org extension.
//! extensions: "rawkode.kanban": settings: boards: api: {columns: [{value: "todo"}]}
//! ```
//!
//! Every root `*.cue` file declaring `package tartan` is part of it, evaluated as CUE evaluates
//! a package directory: with the repository's `cue.mod/` when there is one, imports of packages
//! elsewhere in the module, and registry modules through cuengine's registry configuration.
//! [`load`] reads what the package needs from a [`ConfigSource`]; [`evaluate`] evaluates it.
//!
//! An entry with a `version` installs that version where the configuration lives: on the
//! repository, or on the org for the org's configuration repository. An entry without one
//! contributes settings to the org's installation of that extension.

mod files;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use crate::files::{ConfigFiles, ConfigSource, DirEntry, LIMITS, Limits, load};

/// The CUE package a configuration is.
pub const PACKAGE: &str = "tartan";

/// The repository in each org whose configuration is the org's.
pub const ORG_CONFIG_REPO: &str = "tartan";

/// The schema every configuration is evaluated with, added to the package.
const SCHEMA: &str = r#"package tartan

// Closed, so a key that is not an extension id is an error.
extensions?: close({[=~"^[a-z0-9][a-z0-9._-]{0,63}$"]: close({
	version?:  =~"^[0-9]+\\.[0-9]+\\.[0-9]+([-+][0-9A-Za-z.-]+)?$"
	settings?: {...}
})})
"#;

/// The file the schema is written to; a repository's own file of that name is replaced.
const SCHEMA_FILE: &str = "zz_tartan_schema.cue";

/// The module a configuration without `cue.mod/module.cue` is evaluated as.
const DEFAULT_MODULE: &str = "module: \"config.tartan.dev\"\nlanguage: version: \"v0.14.1\"\n";

/// An evaluated configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Each extension the configuration names, by id.
    #[serde(default)]
    pub extensions: BTreeMap<String, ExtensionEntry>,
}

/// One extension's entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    /// The version to install; `None` contributes settings to the org's installation.
    #[serde(default)]
    pub version: Option<String>,
    /// The settings, as JSON.
    #[serde(default = "empty_object")]
    pub settings: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Why a configuration was refused.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The CUE does not evaluate, does not fit the schema, or is too large.
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
    if !files.contains("cue.mod/module.cue") {
        write(dir.path(), "cue.mod/module.cue", DEFAULT_MODULE)?;
    }
    write(dir.path(), SCHEMA_FILE, SCHEMA)?;
    let options = cuengine::ModuleEvalOptions {
        package_name: Some(PACKAGE.to_owned()),
        ..cuengine::ModuleEvalOptions::default()
    };
    let result = cuengine::evaluate_module(dir.path(), PACKAGE, Some(&options))
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
    let instance =
        result.instances.get(".").cloned().ok_or_else(|| {
            ConfigError::Invalid("the root package tartan did not evaluate".into())
        })?;
    serde_json::from_value(instance).map_err(|error| ConfigError::Invalid(error.to_string()))
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
    use super::*;

    fn files(list: &[(&str, &str)]) -> ConfigFiles {
        ConfigFiles::from_iter(list.iter().map(|(p, c)| ((*p).to_owned(), (*c).to_owned())))
    }

    fn one(source: &str) -> ConfigFiles {
        files(&[("tartan.cue", source)])
    }

    #[tokio::test]
    async fn evaluates_installs_and_contributions() {
        let config = evaluate(one(r#"package tartan

let options = ["todo", "doing", "done"]

extensions: "rawkode.issues": {
	version: "0.5.0"
	settings: fields: status: {type: "select", "options": options, default: options[0]}
}
extensions: "rawkode.kanban": settings: boards: api: columns: [for o in options {value: o}]
"#))
        .await
        .expect("valid");
        let issues = &config.extensions["rawkode.issues"];
        assert_eq!(issues.version.as_deref(), Some("0.5.0"));
        assert_eq!(issues.settings["fields"]["status"]["default"], "todo");
        let kanban = &config.extensions["rawkode.kanban"];
        assert_eq!(kanban.version, None);
        assert_eq!(
            kanban.settings["boards"]["api"]["columns"][2]["value"],
            "done"
        );
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
                "package tartan\nextensions: \"rawkode.kanban\": settings: {}\n",
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
            (
                "cue.mod/module.cue",
                "module: \"example.com/acme\"\nlanguage: version: \"v0.14.1\"\n",
            ),
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
    }

    #[tokio::test]
    async fn refuses_what_does_not_fit() {
        let bad = [
            "package tartan\nextensions: \"rawkode.issues\": version: \"latest\"\n",
            "package tartan\nextensions: \"rawkode.issues\": colour: \"red\"\n",
            "package tartan\nextensions: \"Bad Id\": version: \"1.0.0\"\n",
            "package tartan\nextensions: \"rawkode.issues\": version: \"1.0.0\" & \"2.0.0\"\n",
            "package tartan\nthis is not cue\n",
        ];
        for source in bad {
            assert!(evaluate(one(source)).await.is_err(), "{source}");
        }
    }
}
