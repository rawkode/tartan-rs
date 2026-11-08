//! The forge actor: one per forge. It owns the org directory, the placement directory and the
//! subscriber registry.

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use rivetkit::prelude::*;
use rivetkit::{Handler, action};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actors::apply_schema;
use crate::actors::org::Init as InitOrg;
use crate::event_log::{EVENT_LOG_SCHEMA, ReadEvents, append, read};
use crate::outbox::{Flush, Outbox, SubscriberSource};
use crate::sql::{optional_text, param_optional_text, param_text, text};
use crate::{Envelope, EventPattern, Slug, org_handle};

/// The forge actor.
#[derive(Debug)]
pub struct Forge {
    outbox: Arc<Outbox>,
}

/// The source name of the forge's events.
const SOURCE: &str = "forge";

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS orgs (
        slug TEXT PRIMARY KEY,
        id TEXT NOT NULL UNIQUE,
        pool TEXT,
        ready INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS placements (
        target TEXT PRIMARY KEY,
        pool TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS subscribers (
        name TEXT PRIMARY KEY,
        pattern TEXT NOT NULL
    );
";

/// An org as the forge knows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgInfo {
    /// The org actor's id.
    pub id: String,
    /// The org's slug.
    pub slug: Slug,
    /// The Rivet pool the org actor was created in; `None` is the default pool.
    pub pool: Option<String>,
}

/// A subscriber: who receives events, and which.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriberSpec {
    /// The subscriber actor's key.
    pub name: Slug,
    /// The event types it receives.
    pub pattern: EventPattern,
}

/// Creates an org, or returns the existing org with that slug.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateOrg {
    /// The org's slug.
    pub slug: Slug,
}

impl Action for CreateOrg {
    type Output = OrgInfo;

    const NAME: &'static str = "createOrg";
}

/// Finds an org by slug.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveOrg {
    /// The org's slug.
    pub slug: Slug,
}

impl Action for ResolveOrg {
    type Output = Option<OrgInfo>;

    const NAME: &'static str = "resolveOrg";
}

/// Names the pool new actors for `target` are created in: `org/<org>` covers the org and its
/// repositories, `repo/<org>/<repo>` one repository. `None` removes the entry. Actors that
/// already exist stay where they are.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetPlacement {
    /// `org/<org>` or `repo/<org>/<repo>`.
    pub target: String,
    /// The Rivet pool.
    pub pool: Option<String>,
}

impl Action for SetPlacement {
    type Output = ();

    const NAME: &'static str = "setPlacement";
}

/// The pool for `target`: its own entry, else its org's, else `None` for the default pool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Placement {
    /// `org/<org>` or `repo/<org>/<repo>`.
    pub target: String,
}

impl Action for Placement {
    type Output = Option<String>;

    const NAME: &'static str = "placement";
}

/// Adds a subscriber, or changes its pattern.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterSubscriber {
    /// The subscriber.
    pub subscriber: SubscriberSpec,
}

impl Action for RegisterSubscriber {
    type Output = ();

    const NAME: &'static str = "registerSubscriber";
}

/// Every subscriber.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListSubscribers {}

impl Action for ListSubscribers {
    type Output = Vec<SubscriberSpec>;

    const NAME: &'static str = "listSubscribers";
}

#[async_trait]
impl Actor for Forge {
    type Input = ();
    type Actions = (
        CreateOrg,
        ResolveOrg,
        SetPlacement,
        Placement,
        RegisterSubscriber,
        ListSubscribers,
        ReadEvents,
        Flush,
    );
    type Events = ();
    type ConnParams = ();
    type Action = action::Raw;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        apply_schema(ctx.sql(), EVENT_LOG_SCHEMA).await?;
        apply_schema(ctx.sql(), SCHEMA).await?;
        Ok(Self {
            outbox: Outbox::new(SOURCE.to_owned()),
        })
    }

    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        self.flush_later(&ctx).await;
        Ok(())
    }
}

impl Forge {
    async fn flush_later(&self, ctx: &Ctx<Self>) {
        match subscribers(ctx).await {
            Ok(list) => self.outbox.flush_later(ctx, SubscriberSource::Known(list)),
            Err(error) => tracing::error!(%error, "could not list subscribers"),
        }
    }

    async fn org_by_slug(ctx: &Ctx<Self>, slug: &Slug) -> Result<Option<(OrgInfo, bool)>> {
        let result = ctx
            .sql()
            .query(
                "SELECT id, pool, ready FROM orgs WHERE slug = ?",
                Some(vec![param_text(slug.as_str())]),
            )
            .await?;
        let Some(row) = result.rows.first() else {
            return Ok(None);
        };
        let info = OrgInfo {
            id: text(row, 0)?,
            slug: slug.clone(),
            pool: optional_text(row, 1)?,
        };
        Ok(Some((info, crate::sql::integer(row, 2)? != 0)))
    }

    async fn placement_for(ctx: &Ctx<Self>, target: &str) -> Result<Option<String>> {
        let mut candidates = vec![target.to_owned()];
        if let Some(rest) = target.strip_prefix("repo/")
            && let Some((org, _)) = rest.split_once('/')
        {
            candidates.push(format!("org/{org}"));
        }
        for candidate in candidates {
            let result = ctx
                .sql()
                .query(
                    "SELECT pool FROM placements WHERE target = ?",
                    Some(vec![param_text(candidate)]),
                )
                .await?;
            if let Some(row) = result.rows.first() {
                return Ok(Some(text(row, 0)?));
            }
        }
        Ok(None)
    }
}

impl Handler<CreateOrg> for Forge {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: CreateOrg) -> Result<OrgInfo> {
        let slug = action.slug;
        if Self::org_by_slug(&ctx, &slug).await?.is_none() {
            let pool = Self::placement_for(&ctx, &format!("org/{slug}")).await?;
            let id = ulid::Ulid::new().to_string().to_lowercase();
            // A concurrent create of the same slug keeps the first id.
            ctx.sql()
                .execute(
                    "INSERT OR IGNORE INTO orgs (slug, id, pool) VALUES (?, ?, ?)",
                    Some(vec![
                        param_text(slug.as_str()),
                        param_text(id),
                        param_optional_text(pool),
                    ]),
                )
                .await?;
        }
        let Some((info, ready)) = Self::org_by_slug(&ctx, &slug).await? else {
            bail!("org {slug} vanished while being created");
        };
        if ready {
            return Ok(info);
        }

        // The org actor is initialized first, so a retry after a failure here finds the same
        // pending id and initializes it again, which is idempotent.
        org_handle(&ctx.client()?, &info.id, info.pool.clone())?
            .send(InitOrg {
                id: info.id.clone(),
                slug: slug.clone(),
            })
            .await?;

        let event_info = info.clone();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let marked = tx
                        .execute(
                            "UPDATE orgs SET ready = 1 WHERE slug = ? AND ready = 0",
                            Some(vec![param_text(event_info.slug.as_str())]),
                        )
                        .await?;
                    if marked.changes == 1 {
                        let data = json!({
                            "org_id": event_info.id,
                            "slug": event_info.slug,
                            "pool": event_info.pool,
                        });
                        append(&tx, SOURCE, "org.created", data).await?;
                    }
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("create_org"),
                    timeout: None,
                },
            )
            .await?;
        self.flush_later(&ctx).await;
        Ok(info)
    }
}

impl Handler<ResolveOrg> for Forge {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: ResolveOrg,
    ) -> Result<Option<OrgInfo>> {
        Ok(Self::org_by_slug(&ctx, &action.slug)
            .await?
            .and_then(|(info, ready)| ready.then_some(info)))
    }
}

impl Handler<SetPlacement> for Forge {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: SetPlacement) -> Result<()> {
        if !valid_placement_target(&action.target) {
            bail!(
                "placement target {:?} is not org/<org> or repo/<org>/<repo>",
                action.target
            );
        }
        ctx.sql()
            .transaction(
                move |tx| async move {
                    match &action.pool {
                        Some(pool) => {
                            tx.execute(
                                "INSERT INTO placements (target, pool) VALUES (?, ?)
                                 ON CONFLICT (target) DO UPDATE SET pool = excluded.pool",
                                Some(vec![param_text(action.target.as_str()), param_text(pool)]),
                            )
                            .await?;
                        }
                        None => {
                            tx.execute(
                                "DELETE FROM placements WHERE target = ?",
                                Some(vec![param_text(action.target.as_str())]),
                            )
                            .await?;
                        }
                    }
                    let data = json!({ "target": action.target, "pool": action.pool });
                    append(&tx, SOURCE, "placement.set", data).await?;
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("set_placement"),
                    timeout: None,
                },
            )
            .await?;
        self.flush_later(&ctx).await;
        Ok(())
    }
}

impl Handler<Placement> for Forge {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Placement) -> Result<Option<String>> {
        if !valid_placement_target(&action.target) {
            bail!(
                "placement target {:?} is not org/<org> or repo/<org>/<repo>",
                action.target
            );
        }
        Self::placement_for(&ctx, &action.target).await
    }
}

impl Handler<RegisterSubscriber> for Forge {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: RegisterSubscriber) -> Result<()> {
        let subscriber = action.subscriber;
        ctx.sql()
            .transaction(
                move |tx| async move {
                    tx.execute(
                        "INSERT INTO subscribers (name, pattern) VALUES (?, ?)
                         ON CONFLICT (name) DO UPDATE SET pattern = excluded.pattern",
                        Some(vec![
                            param_text(subscriber.name.as_str()),
                            param_text(subscriber.pattern.as_str()),
                        ]),
                    )
                    .await?;
                    let data = json!({ "name": subscriber.name, "pattern": subscriber.pattern });
                    append(&tx, SOURCE, "subscriber.registered", data).await?;
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("register_subscriber"),
                    timeout: None,
                },
            )
            .await?;
        self.flush_later(&ctx).await;
        Ok(())
    }
}

impl Handler<ListSubscribers> for Forge {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: ListSubscribers,
    ) -> Result<Vec<SubscriberSpec>> {
        subscribers(&ctx).await
    }
}

impl Handler<ReadEvents> for Forge {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ReadEvents) -> Result<Vec<Envelope>> {
        read(ctx.sql(), SOURCE, action.after, action.limit).await
    }
}

impl Handler<Flush> for Forge {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Flush) -> Result<()> {
        let list = subscribers(&ctx).await?;
        self.outbox.flush(&ctx, SubscriberSource::Known(list)).await
    }
}

async fn subscribers(ctx: &Ctx<Forge>) -> Result<Vec<SubscriberSpec>> {
    let result = ctx
        .sql()
        .query("SELECT name, pattern FROM subscribers ORDER BY name", None)
        .await?;
    result
        .rows
        .iter()
        .map(|row| {
            Ok(SubscriberSpec {
                name: text(row, 0)?.parse()?,
                pattern: text(row, 1)?.parse()?,
            })
        })
        .collect()
}

/// Whether `target` names a placement: `org/<org>` or `repo/<org>/<repo>`.
pub fn valid_placement_target(target: &str) -> bool {
    let parts: Vec<&str> = target.split('/').collect();
    let slugs_valid = |slugs: &[&str]| slugs.iter().all(|s| s.parse::<Slug>().is_ok());
    match parts.as_slice() {
        ["org", org] => slugs_valid(&[org]),
        ["repo", org, repo] => slugs_valid(&[org, repo]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::valid_placement_target;

    #[test]
    fn placement_targets() {
        assert!(valid_placement_target("org/acme"));
        assert!(valid_placement_target("repo/acme/api"));
        assert!(!valid_placement_target("acme"));
        assert!(!valid_placement_target("repo/acme"));
        assert!(!valid_placement_target("org/Acme"));
    }
}
