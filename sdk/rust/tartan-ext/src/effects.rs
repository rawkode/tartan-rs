//! The `effects` import. The host buffers each effect and applies it through
//! the kernel's capabilities only after the export returns `Ok`; an export
//! that fails applies none.
//!
//! Checked at once (the import returns the error): a read-only call
//! (`render`, `provide-context`) gets `denied("read-only")` for every
//! effect, a shadow installation `denied("shadow")` for notices, and a
//! missing manifest permission `denied("grant")` (`notify`, `notes`).
//! Checked when the host applies the buffer, after the hook returned: the
//! event type (`x.<ext id>.*` or an interface event the package provides),
//! the notice's recipient and the rate limit. A failure there fails the
//! call, so an event delivery is retried and a gate falls back to its
//! declared default.

use crate::bindings::tartan::ext::effects as raw;
use crate::{EntityRef, Error, NotifyOptions};

/// Appends an event (`x.<ext id>.<name>` or an interface event the package
/// provides) to the installation's stream.
pub fn emit(kind: &str, subject: Option<&EntityRef>, data: &serde_json::Value) -> Result<(), Error> {
	raw::emit(kind, subject, &data.to_string())
}

/// Sends a notice to a principal with a role in the installation subtree.
pub fn notify(
	principal: &str,
	options: &NotifyOptions,
	text: &str,
	data: &serde_json::Value,
) -> Result<(), Error> {
	raw::notify(principal, options, text, &data.to_string())
}

/// Contributes a section (≤ 8 KB of JSON) to a change's why note.
pub fn contribute_note(change_id: &str, section: &serde_json::Value) -> Result<(), Error> {
	raw::contribute_note(change_id, &section.to_string())
}

/// Sets (or moves) the timer `key`; `on_timer(key)` runs at or after `at_ms`.
pub fn set_timer(key: &str, at_ms: u64) {
	raw::set_timer(key, at_ms);
}
