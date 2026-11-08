//! Tartan's actors.
//!
//! Each tier of the forge is an actor type with its own SQLite database:
//!
//! - [`Forge`], one per forge: orgs, placements and event subscribers;
//! - [`Org`], one per org: its repositories;
//! - [`Repo`], one per repository: the repository's forge state and the events of its refs;
//! - [`Subscriber`], one per subscription: the events it has received.
//!
//! Every state change appends an [`Envelope`] to the changing actor's event log in the same
//! transaction. An outbox in each actor then delivers the events matching each subscriber's
//! [`EventPattern`], in order and at least once. Git hosting itself is delegated to a
//! [`GitHost`], walgit in production.

mod actors;
mod clients;
mod envelope;
mod event_log;
mod event_pattern;
mod outbox;
mod services;
mod slug;
mod sql;
mod walgit;

pub use crate::actors::forge::{
    CreateOrg, Forge, ListSubscribers, OrgInfo, Placement, RegisterSubscriber, ResolveOrg,
    SetPlacement, SubscriberSpec, valid_placement_target,
};
pub use crate::actors::org::{CreateRepo, Org, OrgStatus, OrgStatusQuery, RepoInfo, ResolveRepo};
pub use crate::actors::repo::{RecordRefEvents, Repo, RepoStatus, RepoStatusQuery};
pub use crate::actors::subscriber::{Received, ReceivedEvent, Subscriber};
pub use crate::clients::{forge_handle, org_handle, repo_handle, subscriber_handle};
pub use crate::envelope::Envelope;
pub use crate::event_log::ReadEvents;
pub use crate::event_pattern::{EventPattern, EventPatternError};
pub use crate::services::{GitHost, Services, install_services};
pub use crate::slug::{Slug, SlugError};
pub use crate::walgit::{RefEvent, WalgitClient, verify_signature};

use rivetkit::Registry;

/// Registers every Tartan actor type with `registry`.
pub fn register_actors(registry: &mut Registry) {
    registry.register_actor::<Forge>(actors::FORGE);
    registry.register_actor::<Org>(actors::ORG);
    registry.register_actor::<Repo>(actors::REPO);
    registry.register_actor::<Subscriber>(actors::SUBSCRIBER);
}
