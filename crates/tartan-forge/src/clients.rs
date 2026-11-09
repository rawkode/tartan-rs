//! Typed handles to the actors, for the gateway and for actors calling each other.

use anyhow::Result;
use rivetkit::TypedActorHandle;
use rivetkit::TypedClientExt;
use rivetkit::client::{Client, GetOrCreateOptions};

use crate::actors::{EXTENSION, FORGE, IDENTITY, ORG, REPO, SUBSCRIBER};
use crate::services::services;
use crate::{Extension, Forge, Identity, Org, Repo, Subscriber};

/// The forge's single [`Forge`] actor.
pub fn forge_handle(client: &Client) -> Result<TypedActorHandle<Forge>> {
    let forge_id = services()?.forge_id.as_str();
    client.get_or_create_typed::<Forge>(FORGE, [forge_id], GetOrCreateOptions::default())
}

/// The forge's single [`Identity`] actor. It always runs in the default pool.
pub fn identity_handle(client: &Client) -> Result<TypedActorHandle<Identity>> {
    let forge_id = services()?.forge_id.as_str();
    client.get_or_create_typed::<Identity>(IDENTITY, [forge_id], GetOrCreateOptions::default())
}

/// The [`Org`] actor `org_id`, created in `pool` (the client's pool when `None`) if it does not
/// exist yet.
pub fn org_handle(
    client: &Client,
    org_id: &str,
    pool: Option<String>,
) -> Result<TypedActorHandle<Org>> {
    client.get_or_create_typed::<Org>(ORG, [org_id], in_pool(pool))
}

/// The [`Repo`] actor `repo_id`, created in `pool` if it does not exist yet.
pub fn repo_handle(
    client: &Client,
    repo_id: &str,
    pool: Option<String>,
) -> Result<TypedActorHandle<Repo>> {
    client.get_or_create_typed::<Repo>(REPO, [repo_id], in_pool(pool))
}

/// The [`Extension`] actor of installation `installation_id`, created in `pool` if it does not
/// exist yet.
pub fn extension_handle(
    client: &Client,
    installation_id: &str,
    pool: Option<String>,
) -> Result<TypedActorHandle<Extension>> {
    client.get_or_create_typed::<Extension>(EXTENSION, [installation_id], in_pool(pool))
}

/// The [`Subscriber`] actor `name` of this forge.
pub fn subscriber_handle(client: &Client, name: &str) -> Result<TypedActorHandle<Subscriber>> {
    let forge_id = services()?.forge_id.as_str();
    client.get_or_create_typed::<Subscriber>(
        SUBSCRIBER,
        [forge_id, name],
        GetOrCreateOptions::default(),
    )
}

fn in_pool(pool: Option<String>) -> GetOrCreateOptions {
    GetOrCreateOptions {
        pool_name: pool,
        ..GetOrCreateOptions::default()
    }
}
