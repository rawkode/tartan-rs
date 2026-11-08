//! A repository's configuration: the root `tartan.cue`, CUE package `tartan`.
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
//! An entry with a `version` installs that version where the file lives: on the repository, or
//! on the org for the org's configuration repository. An entry without one contributes settings
//! to the org's installation of that extension. Configuration is the only way extensions are
//! installed: removing an entry uninstalls it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The configuration file at a repository's root.
pub const CONFIG_FILE: &str = "tartan.cue";

/// The repository in each org whose configuration is the org's.
pub const ORG_CONFIG_REPO: &str = "tartan";

/// The schema every configuration is evaluated with, in the same package.
const SCHEMA: &str = r#"package tartan

// Closed, so a key that is not an extension id is an error.
extensions?: close({[=~"^[a-z0-9][a-z0-9._-]{0,63}$"]: close({
	version?:  =~"^[0-9]+\\.[0-9]+\\.[0-9]+([-+][0-9A-Za-z.-]+)?$"
	settings?: {...}
})})
"#;

/// The CUE language version the configuration is evaluated as.
const MODULE: &str = "module: \"config.tartan.dev\"\nlanguage: version: \"v0.14.1\"\n";

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
    /// The CUE does not evaluate, or does not fit the schema.
    #[error("{0}")]
    Invalid(String),
    /// The evaluator could not run.
    #[error("the CUE evaluator failed: {0}")]
    Evaluator(String),
}

/// Evaluates `source`, the text of a `tartan.cue`. An absent file is the empty configuration,
/// which installs nothing.
pub async fn evaluate(source: Option<&str>) -> Result<Config, ConfigError> {
    let Some(source) = source else {
        return Ok(Config::default());
    };
    let source = source.to_owned();
    tokio::task::spawn_blocking(move || evaluate_blocking(&source))
        .await
        .map_err(|error| ConfigError::Evaluator(error.to_string()))?
}

fn evaluate_blocking(source: &str) -> Result<Config, ConfigError> {
    let dir = tempfile::tempdir().map_err(|error| ConfigError::Evaluator(error.to_string()))?;
    write(dir.path(), "cue.mod/module.cue", MODULE)?;
    write(dir.path(), "zz_tartan_schema.cue", SCHEMA)?;
    write(dir.path(), CONFIG_FILE, source)?;
    let json = cuengine::evaluate_cue_package(dir.path(), "tartan")
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
    serde_json::from_str(&json).map_err(|error| ConfigError::Invalid(error.to_string()))
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

    #[tokio::test]
    async fn evaluates_installs_and_contributions() {
        let config = evaluate(Some(
            r#"package tartan

let options = ["todo", "doing", "done"]

extensions: "rawkode.issues": {
	version: "0.5.0"
	settings: fields: status: {type: "select", "options": options, default: options[0]}
}
extensions: "rawkode.kanban": settings: boards: api: columns: [for o in options {value: o}]
"#,
        ))
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
    async fn an_absent_file_installs_nothing() {
        assert_eq!(evaluate(None).await.expect("valid"), Config::default());
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
            assert!(evaluate(Some(source)).await.is_err(), "{source}");
        }
    }
}
