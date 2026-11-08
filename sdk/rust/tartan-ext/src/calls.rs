//! The `calls` import: another extension's tools, as the principal the current call acts for.
//! The manifest must name each one in `permissions.calls`, such as
//! `{"rawkode.issues": ["update"]}`; the host refuses the rest with `denied`, and refuses every
//! call during `render` and `provide-context`.

use crate::bindings::tartan::ext::calls as raw;
use crate::{Error, serde_json};

/// Calls `tool` of `extension`, installed on repository `repo` (its id), with `args`.
pub fn call(
	extension: &str,
	repo: Option<&str>,
	tool: &str,
	args: &serde_json::Value,
) -> Result<serde_json::Value, Error> {
	let reply = raw::call(extension, repo, tool, &args.to_string())?;
	serde_json::from_str(&reply).map_err(|e| Error::internal(format!("{extension} {tool}: {e}")))
}
