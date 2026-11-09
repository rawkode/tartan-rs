//! # tartan-ext
//!
//! The Rust SDK for Tartan extensions (`tartan:ext@0.1.0`).
//! An extension is a WebAssembly component: the forge runs it in wasmtime,
//! one instance per call, against the installation's own SQLite database.
//!
//! WIT has no optional exports, so this crate supplies them: implement
//! [`Extension`] (every hook has a default) and call [`export!`]:
//!
//! ```ignore
//! use tartan_ext::{export, ui, Extension, SlotContext, Error, Json};
//!
//! struct Hello;
//!
//! impl Extension for Hello {
//!     fn render(slot: String, _ctx: SlotContext, _props: Json) -> Result<Json, Error> {
//!         Ok(ui::doc(ui::text(format!("hello from {slot}"))).to_string())
//!     }
//! }
//!
//! export!(Hello);
//! ```
//!
//! Payloads and UI trees cross the boundary as JSON strings ([`Json`]); the
//! [`ui`] builders produce `tartan-ui@1` nodes, [`sql`] wraps the facet's
//! synchronous SQLite, [`host`] has the log, the installation config, the
//! clock, entropy and ULIDs, and [`effects`] buffers events, notices, note
//! sections and timers, which the host applies through the kernel's
//! capabilities only after the export returns `Ok` (and never during
//! `render` or `provide-context`, which are read-only), and [`calls`] reaches
//! the tools of other extensions the manifest names.

pub mod calls;
pub mod did;
pub mod effects;
pub mod gate;
pub mod host;
pub mod sql;
pub mod ui;

/// The bindings generated from `wit/tartan.wit`.
pub mod bindings {
	wit_bindgen::generate!({
		path: "../../../wit",
		world: "tartan:ext/extension",
		pub_export_macro: true,
		export_macro_name: "__export_tartan_extension",
		default_bindings_module: "::tartan_ext::bindings",
	});
}

pub use bindings::tartan::ext::kv;
pub use bindings::tartan::ext::types::{
	Actor, ActorKind, EntityRef, Error, Event, GateDecision, InstallMode, NotifyOptions, Severity,
	SlotContext, Verdict,
};
pub use serde_json;

/// A JSON document as a string (WIT has no recursive types).
pub type Json = String;

impl Error {
	/// `denied(reason)`: `"scope"`, `"read-only"`, `"shadow"`, `"grant"`, …
	pub fn denied(reason: impl Into<String>) -> Self {
		Error::Denied(reason.into())
	}
	pub fn not_found(what: impl Into<String>) -> Self {
		Error::NotFound(what.into())
	}
	pub fn invalid(message: impl Into<String>) -> Self {
		Error::Invalid(message.into())
	}
	pub fn conflict(message: impl Into<String>) -> Self {
		Error::Conflict(message.into())
	}
	pub fn unavailable(message: impl Into<String>) -> Self {
		Error::Unavailable(message.into())
	}
	pub fn internal(message: impl Into<String>) -> Self {
		Error::Internal(message.into())
	}
}

/// Parses a JSON payload, mapping a syntax error to `invalid`.
pub fn parse_json(input: &str) -> Result<serde_json::Value, Error> {
	serde_json::from_str(input).map_err(|e| Error::invalid(format!("invalid JSON: {e}")))
}

/// The hooks of an extension. Every method has a default, so implement only
/// the ones the manifest declares (a gate the manifest names, its slots, its
/// tools). The host calls a hook only when the manifest declares it.
pub trait Extension {
	/// Once per installed version, after the package's migrations ran.
	fn init() -> Result<(), Error> {
		Ok(())
	}

	/// A subscribed event (manifest `subscribe`); runs as the installation.
	fn on_event(_event: Event) -> Result<(), Error> {
		Ok(())
	}

	/// A timer set with [`effects::set_timer`] fired.
	fn on_timer(_key: String) -> Result<(), Error> {
		Ok(())
	}

	/// A declared gate (`ref.advance`, `lane.open`, `push`); `input` is the
	/// kernel's prefetched gate input as JSON.
	fn gate(point: String, _input: Json, _ctx: SlotContext) -> Result<GateDecision, Error> {
		Err(Error::not_found(format!("gate {point}")))
	}

	/// `push.accepted` echo: up to 10 `remote:` lines (sanitized by the kernel).
	fn echo(_event: Event, _input: Json) -> Result<Vec<String>, Error> {
		Ok(Vec::new())
	}

	/// A dynamic slot: returns a `tartan-ui@1` document ([`ui::doc`]).
	/// Read-only: SQL is SELECT-only and every effect is denied.
	fn render(slot: String, _ctx: SlotContext, _props: Json) -> Result<Json, Error> {
		Err(Error::not_found(format!("slot {slot}")))
	}

	/// A slot action (button, form submit): returns an action result
	/// ([`ui::result`]).
	fn on_action(action: String, _payload: Json, _ctx: SlotContext) -> Result<Json, Error> {
		Err(Error::not_found(format!("action {action}")))
	}

	/// A contributed tool (`contributes.tools`); `args` matched its schema.
	fn call_tool(name: String, _args: Json, _ctx: SlotContext) -> Result<Json, Error> {
		Err(Error::not_found(format!("tool {name}")))
	}

	/// Context sections for `context_get` (a JSON array). Read-only.
	fn provide_context(_request: Json) -> Result<Json, Error> {
		Ok("[]".to_string())
	}
}

impl<T: Extension> bindings::Guest for T {
	fn init() -> Result<(), Error> {
		T::init()
	}
	fn on_event(e: Event) -> Result<(), Error> {
		T::on_event(e)
	}
	fn on_timer(key: String) -> Result<(), Error> {
		T::on_timer(key)
	}
	fn gate(point: String, input: Json, ctx: SlotContext) -> Result<GateDecision, Error> {
		T::gate(point, input, ctx)
	}
	fn echo(e: Event, input: Json) -> Result<Vec<String>, Error> {
		T::echo(e, input)
	}
	fn render(slot: String, ctx: SlotContext, props: Json) -> Result<Json, Error> {
		T::render(slot, ctx, props)
	}
	fn on_action(action: String, payload: Json, ctx: SlotContext) -> Result<Json, Error> {
		T::on_action(action, payload, ctx)
	}
	fn call_tool(name: String, args: Json, ctx: SlotContext) -> Result<Json, Error> {
		T::call_tool(name, args, ctx)
	}
	fn provide_context(req: Json) -> Result<Json, Error> {
		T::provide_context(req)
	}
}

/// Exports an [`Extension`] implementation as the component's world.
#[macro_export]
macro_rules! export {
	($ty:ident) => {
		$crate::bindings::__export_tartan_extension!($ty with_types_in $crate::bindings);
	};
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn error_constructors_carry_the_text() {
		assert!(matches!(Error::denied("scope"), Error::Denied(s) if s == "scope"));
		assert!(matches!(Error::invalid("bad"), Error::Invalid(s) if s == "bad"));
		assert!(matches!(Error::not_found("x"), Error::NotFound(s) if s == "x"));
	}

	#[test]
	fn parse_json_maps_syntax_errors_to_invalid() {
		assert_eq!(parse_json("{\"a\":1}").unwrap()["a"], 1);
		assert!(matches!(parse_json("{"), Err(Error::Invalid(_))));
	}

	struct Defaults;
	impl Extension for Defaults {}

	#[test]
	fn defaults_answer_without_host_calls() {
		assert!(<Defaults as Extension>::init().is_ok());
		assert_eq!(
			<Defaults as Extension>::provide_context("{}".into()).unwrap(),
			"[]"
		);
		assert!(matches!(
			<Defaults as Extension>::call_tool("scan".into(), "{}".into(), ctx()),
			Err(Error::NotFound(_))
		));
	}

	pub(crate) fn ctx() -> SlotContext {
		SlotContext {
			slot: None,
			node: "n".into(),
			repo: None,
			git_ref: None,
			path: None,
			entity: None,
			viewer: None,
			mode: InstallMode::Enforce,
			extra: None,
		}
	}
}
