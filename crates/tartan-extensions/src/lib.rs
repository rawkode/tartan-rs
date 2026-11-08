//! Runs Tartan extensions.
//!
//! An extension is a WebAssembly component of the `tartan:ext@0.1.0` world (`wit/tartan.wit`),
//! packaged with a manifest (`tartan.json`) and SQL migrations. [`ExtensionRuntime`] compiles
//! components and calls their exports, one fresh instance per call, each with its own fuel,
//! memory and time limits. The host side of the world is backed by:
//!
//! - an [`SqlBackend`]: the installation's own SQLite database. Statements from the extension pass
//!   a guard that keeps them off the host's `_tartan_*` tables and refuses `ATTACH` and `PRAGMA`;
//!   render and context calls may only read;
//! - the installation's configuration, a clock, randomness and ULIDs;
//! - a [`ToolCaller`] for calls to other installations' tools, which the caller checks against
//!   the manifest's `permissions.calls`;
//! - an effects buffer: events the call emits and timers it sets are returned with its result,
//!   so the caller applies them only when the call succeeds.

mod bindings;
mod host_state;
mod migrations;
mod package;
mod runtime;
mod sql;

pub use crate::bindings::types::{
    Actor, ActorKind, EntityRef, Error as ExtensionError, Event, GateDecision, InstallMode,
    SlotContext, Verdict,
};
pub use crate::host_state::{CallKind, Effect};
pub use crate::migrations::apply_migrations;
pub use crate::package::{
    COMPONENT_FILE, ExtensionPackage, Manifest, PackageError, Storage, StorageScope, Subscription,
};
pub use crate::runtime::{
    CallEnvironment, CallLimits, CallOutcome, ExtensionComponent, ExtensionRuntime,
};
pub use crate::sql::{SqlBackend, SqlRows, SqlValue, ToolCaller, check_statement};

/// The extension API this host implements.
pub const EXTENSION_API: &str = "tartan:ext@0.1.0";
