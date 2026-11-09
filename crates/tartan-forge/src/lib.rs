//! Tartan's actors.
//!
//! Each tier of the forge is an actor type with its own SQLite database:
//!
//! - [`Forge`], one per forge: orgs, placements and event subscribers;
//! - [`Org`], one per org: its repositories;
//! - [`Repo`], one per repository: the repository's forge state and the events of its refs;
//! - [`Subscriber`], one per subscription: the events it has received;
//! - [`Identity`], one per forge: people, sessions, access tokens and signature records.
//!
//! Every state change appends an [`Envelope`] to the changing actor's event log in the same
//! transaction. An outbox in each actor then delivers the events matching each subscriber's
//! [`EventPattern`], in order and at least once. Git hosting itself is delegated to a
//! [`GitHost`], walgit in production.

mod actors;
mod clients;
mod clock;
mod envelope;
mod event_log;
mod event_pattern;
mod extensions;
mod outbox;
mod services;
mod slug;
mod sql;
mod walgit;

pub use crate::actors::extension::{
    Act, CallReply, CallTool, DeliverEvents, Disable, Extension, InstallationSpec, Render,
};
pub use crate::actors::forge::{
    CreateOrg, Forge, ListOrgs, ListPackages, ListRecipients, ListSubscribers, OrgInfo,
    PackageInfo, PackageQuery, Placement, PublishPackage, Recipient, RegisterSubscriber,
    ResolveOrg, SetPlacement, SubscribeInstallation, SubscriberSpec, valid_placement_target,
};
pub use crate::actors::identity::Identity;
pub use crate::actors::installations::{InstallationInfo, InstallationQuery, ListInstallations};
pub use crate::actors::org::{
    CreateRepo, ListRepos, Org, OrgStatus, OrgStatusQuery, RepoInfo, ResolveRepo,
};
pub use crate::actors::repo::{RecordRefEvents, Repo, RepoStatus, RepoStatusQuery};
pub use crate::actors::subscriber::{Received, ReceivedEvent, Subscriber};
pub use crate::clients::{
    extension_handle, forge_handle, identity_handle, org_handle, repo_handle, subscriber_handle,
};
pub use crate::clock::{Clock, SystemClock, real_time_ms};
pub use crate::envelope::Envelope;
pub use crate::event_log::ReadEvents;
pub use crate::event_pattern::{EventPattern, EventPatternError};
pub use crate::extensions::{Extensions, Loaded, RegisterError, register_package};
pub use crate::outbox::{Flush, FlushTree};
pub use crate::services::{GitHost, Services, install_services};
pub use crate::slug::{Slug, SlugError};
pub use crate::walgit::{RefEvent, WalgitClient, verify_signature};

use rivetkit::Registry;

/// Registers every Tartan actor type with `registry`.
pub fn register_actors(registry: &mut Registry) {
    registry.register_actor::<Forge>(actors::FORGE);
    registry.register_actor::<Identity>(actors::IDENTITY);
    registry.register_actor::<Org>(actors::ORG);
    registry.register_actor::<Repo>(actors::REPO);
    registry.register_actor::<Subscriber>(actors::SUBSCRIBER);
    registry.register_actor::<Extension>(actors::EXTENSION);
}
