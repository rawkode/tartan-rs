//! The Tartan forge's HTTP gateway.
//!
//! - `/-/api/…`: the forge API (orgs, repositories, placements, subscribers), behind the admin
//!   bearer token.
//! - `/-/hooks/git-host`: ref events from the Git host, signed with the shared webhook secret.

mod api_error;
mod gateway;

pub use crate::api_error::ApiError;
pub use crate::gateway::{GatewayState, router};
