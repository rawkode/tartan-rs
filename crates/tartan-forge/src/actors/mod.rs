//! The actor types and what they share.

pub(crate) mod extension;
pub(crate) mod forge;
pub(crate) mod identity;
pub(crate) mod installations;
pub(crate) mod org;
pub(crate) mod repo;
pub(crate) mod subscriber;

use anyhow::Result;
use rivetkit::SqliteDb;

/// The registered name of the [`extension::Extension`] actor type.
pub(crate) const EXTENSION: &str = "extension";
/// The registered name of the [`forge::Forge`] actor type.
pub(crate) const FORGE: &str = "forge";
/// The registered name of the [`identity::Identity`] actor type.
pub(crate) const IDENTITY: &str = tartan_identity_api::ACTOR_NAME;
/// The registered name of the [`org::Org`] actor type.
pub(crate) const ORG: &str = "org";
/// The registered name of the [`repo::Repo`] actor type.
pub(crate) const REPO: &str = "repo";
/// The registered name of the [`subscriber::Subscriber`] actor type.
pub(crate) const SUBSCRIBER: &str = "subscriber";

/// Runs each `;`-separated statement of `schema`.
pub(crate) async fn apply_schema(db: &SqliteDb, schema: &str) -> Result<()> {
    for statement in schema.split(';') {
        if !statement.trim().is_empty() {
            db.exec(statement).await?;
        }
    }
    Ok(())
}

/// The first segment of the actor's key, which every Tartan actor is addressed by.
pub(crate) fn key_id(key: &rivetkit::ActorKey) -> Result<String> {
    match key.first() {
        Some(rivetkit::ActorKeySegment::String(id)) => Ok(id.clone()),
        other => Err(anyhow::anyhow!(
            "expected a string actor key, got {other:?}"
        )),
    }
}
