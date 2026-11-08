//! The `host` import: the console log, the installation config, the clock,
//! entropy and ULIDs (`wasm32-unknown-unknown` has none of its own).
//!
//! Log lines go to the installation's console (Extensions → installation),
//! tagged with the installation and the hook by the host.

use crate::bindings::tartan::ext::host as raw;

pub use raw::Level;

pub fn log(level: Level, message: &str) {
	raw::log(level, message);
}

pub fn debug(message: &str) {
	raw::log(Level::Debug, message);
}

pub fn info(message: &str) {
	raw::log(Level::Info, message);
}

pub fn warn(message: &str) {
	raw::log(Level::Warn, message);
}

pub fn error(message: &str) {
	raw::log(Level::Error, message);
}

/// The installation config (the settings form, merged over the manifest's
/// `config.default`), or `{}` when it does not parse.
pub fn config() -> serde_json::Value {
	serde_json::from_str(&raw::config()).unwrap_or_else(|_| serde_json::json!({}))
}

/// Milliseconds since the Unix epoch, from the host clock.
pub fn now_ms() -> u64 {
	raw::now_ms()
}

/// `len` random bytes from the host.
pub fn random(len: u32) -> Vec<u8> {
	raw::random(len)
}

/// A fresh ULID from the host.
pub fn ulid() -> String {
	raw::ulid()
}
