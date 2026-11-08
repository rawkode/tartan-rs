//! The Tartan forge's HTTP gateway.
//!
//! - `/-/api/…`: the forge API (orgs, repositories, placements, subscribers), behind the admin
//!   bearer token.
//! - `/-/api/packages` and `/-/api/orgs/{org}/repos/{repo}/extensions/…`: extension packages,
//!   installations and calls into them.
//! - `/-/hooks/git-host`: ref events from the Git host, signed with the shared webhook secret.

mod api_error;
mod extension_routes;
mod gateway;

pub use crate::api_error::ApiError;
pub use crate::gateway::{GatewayState, router};
