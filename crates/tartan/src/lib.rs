//! The Tartan forge's HTTP gateway, its CUE registry and the module sync that fills it.
//!
//! - `/-/api/…`: the forge API (orgs, repositories, placements, subscribers, packages, listing
//!   and calling a repository's extensions, and the caller's own account), behind
//!   [`access::authenticate`].
//! - `/-/auth/…`: ATProto OAuth sign-in ([`auth`]), and `tt`'s client metadata and CLI
//!   hand-off ([`cli_auth`]).
//! - `/-/hooks/git-host`: ref events from the Git host, signed with the shared webhook secret.
//! - `/v2/…`: the forge's read-only CUE registry ([`registry`]), filled at startup by
//!   [`sync::sync_modules`] from the modules the deployment configuration lists.
//!
//! Each gateway process also runs the [`jetstream`] task, which turns Jetstream events into
//! identity refreshes and signature re-proofs.

pub mod access;
mod account_routes;
mod api_error;
pub mod auth;
pub mod cli_auth;
pub mod client_ip;
mod extension_routes;
mod gateway;
pub mod jetstream;
mod people_routes;
pub mod registry;
pub mod settings;
pub mod sync;

pub use crate::access::{Access, AccessConfig, Principal, Rights};
pub use crate::api_error::ApiError;
pub use crate::gateway::{GatewayState, router};
pub use crate::registry::RegistryStore;
pub use crate::sync::{Problem, SyncReport, SyncTargets, sync_modules};
