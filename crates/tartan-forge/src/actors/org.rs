//! The org actor: one per org. It owns the org's repository directory.

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use rivetkit::prelude::*;
use rivetkit::{Handler, action};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actors::installations::{
    self, INSTALLATIONS_SCHEMA, Install, InstallTarget, InstallationInfo, InstallationQuery,
    ListInstallations,
};
use crate::actors::repo::Init as InitRepo;
use crate::actors::{apply_schema, key_id};
use crate::event_log::{EVENT_LOG_SCHEMA, ReadEvents, append, read};
use crate::outbox::{Flush, FlushTree, Outbox, SubscriberSource};
use crate::services::services;
use crate::sql::{integer, optional_text, param_optional_text, param_text, text};
use crate::{Envelope, Placement, RecipientScope, Slug, forge_handle, repo_handle};

/// The org actor.
#[derive(Debug)]
pub struct Org {
    id: String,
    outbox: Arc<Outbox>,
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS org (
        id TEXT PRIMARY KEY,
        slug TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS repos (
        slug TEXT PRIMARY KEY,
        id TEXT NOT NULL UNIQUE,
        pool TEXT,
        ready INTEGER NOT NULL DEFAULT 0
    );
";

/// A repository as its org knows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    /// The repository actor's id.
    pub id: String,
    /// The org actor's id.
    pub org_id: String,
    /// The org's slug.
    pub org_slug: Slug,
    /// The repository's slug.
    pub slug: Slug,
    /// The Rivet pool the repository actor was created in; `None` is the default pool.
    pub pool: Option<String>,
    /// The URL to clone it from.
    pub clone_url: String,
}

/// Where an org actor runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgStatus {
    /// The org actor's id.
    pub id: String,
    /// The org's slug.
    pub slug: Slug,
    /// The Rivet pool hosting the actor.
    pub pool: Option<String>,
}

/// Records the org's identity. Sent by the forge actor; repeating it changes nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Init {
    /// The org actor's id, which is also its key.
    pub id: String,
    /// The org's slug.
    pub slug: Slug,
}

impl Action for Init {
    type Output = ();

    const NAME: &'static str = "init";
}

/// Creates a repository in the org, or returns the existing one with that slug.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateRepo {
    /// The repository's slug.
    pub slug: Slug,
}

impl Action for CreateRepo {
    type Output = RepoInfo;

    const NAME: &'static str = "createRepo";
}

/// Finds a repository of the org by slug.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveRepo {
    /// The repository's slug.
    pub slug: Slug,
}

impl Action for ResolveRepo {
    type Output = Option<RepoInfo>;

    const NAME: &'static str = "resolveRepo";
}

/// Where the org actor runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OrgStatusQuery {}

impl Action for OrgStatusQuery {
    type Output = OrgStatus;

    const NAME: &'static str = "status";
}

#[async_trait]
impl Actor for Org {
    type Input = ();
    type Actions = (
        Init,
        CreateRepo,
        ResolveRepo,
        OrgStatusQuery,
        Install,
        InstallationQuery,
        ListInstallations,
        ReadEvents,
        Flush,
        FlushTree,
    );
    type Events = ();
    type ConnParams = ();
    type Action = action::Raw;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        apply_schema(ctx.sql(), EVENT_LOG_SCHEMA).await?;
        apply_schema(ctx.sql(), SCHEMA).await?;
        apply_schema(ctx.sql(), INSTALLATIONS_SCHEMA).await?;
        let id = key_id(ctx.key())?;
        let outbox = Outbox::new(format!("org:{id}"));
        outbox.set_org(id.clone());
        Ok(Self { outbox, id })
    }

    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        Ok(())
    }
}

impl Org {
    fn source(&self) -> String {
        format!("org:{}", self.id)
    }

    async fn slug(&self, ctx: &Ctx<Self>) -> Result<Slug> {
        let result = ctx.sql().query("SELECT slug FROM org", None).await?;
        match result.rows.first() {
            Some(row) => Ok(text(row, 0)?.parse()?),
            None => bail!("org {} has not been initialized", self.id),
        }
    }

    async fn repo_by_slug(
        &self,
        ctx: &Ctx<Self>,
        org_slug: &Slug,
        slug: &Slug,
    ) -> Result<Option<(RepoInfo, bool)>> {
        let result = ctx
            .sql()
            .query(
                "SELECT id, pool, ready FROM repos WHERE slug = ?",
                Some(vec![param_text(slug.as_str())]),
            )
            .await?;
        let Some(row) = result.rows.first() else {
            return Ok(None);
        };
        let info = RepoInfo {
            id: text(row, 0)?,
            org_id: self.id.clone(),
            org_slug: org_slug.clone(),
            slug: slug.clone(),
            pool: optional_text(row, 1)?,
            clone_url: services()?
                .git_host
                .clone_url(org_slug.as_str(), slug.as_str()),
        };
        Ok(Some((info, integer(row, 2)? != 0)))
    }
}

impl Handler<Init> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Init) -> Result<()> {
        if action.id != self.id {
            bail!("init for org {} sent to org {}", action.id, self.id);
        }
        let source = self.source();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let inserted = tx
                        .execute(
                            "INSERT OR IGNORE INTO org (id, slug) VALUES (?, ?)",
                            Some(vec![
                                param_text(action.id.as_str()),
                                param_text(action.slug.as_str()),
                            ]),
                        )
                        .await?;
                    if inserted.changes == 1 {
                        let data = json!({ "org_id": action.id, "slug": action.slug });
                        append(&tx, &source, "org.ready", data).await?;
                    }
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("init_org"),
                    timeout: None,
                },
            )
            .await?;
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        Ok(())
    }
}

impl Handler<CreateRepo> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: CreateRepo) -> Result<RepoInfo> {
        let org_slug = self.slug(&ctx).await?;
        let slug = action.slug;
        if self.repo_by_slug(&ctx, &org_slug, &slug).await?.is_none() {
            let client = ctx.client()?;
            let target = format!("repo/{org_slug}/{slug}");
            let pool = forge_handle(&client)?.send(Placement { target }).await?;
            let id = ulid::Ulid::new().to_string().to_lowercase();
            ctx.sql()
                .execute(
                    "INSERT OR IGNORE INTO repos (slug, id, pool) VALUES (?, ?, ?)",
                    Some(vec![
                        param_text(slug.as_str()),
                        param_text(id),
                        param_optional_text(pool),
                    ]),
                )
                .await?;
        }
        let Some((info, ready)) = self.repo_by_slug(&ctx, &org_slug, &slug).await? else {
            bail!("repository {org_slug}/{slug} vanished while being created");
        };
        if ready {
            return Ok(info);
        }

        // As with orgs: initialize the repository actor first, then mark it ready, so a retry
        // reuses the pending id.
        repo_handle(&ctx.client()?, &info.id, info.pool.clone())?
            .send(InitRepo {
                id: info.id.clone(),
                org_id: self.id.clone(),
                org_slug: org_slug.clone(),
                slug: slug.clone(),
            })
            .await?;

        let source = self.source();
        let event_info = info.clone();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let marked = tx
                        .execute(
                            "UPDATE repos SET ready = 1 WHERE slug = ? AND ready = 0",
                            Some(vec![param_text(event_info.slug.as_str())]),
                        )
                        .await?;
                    if marked.changes == 1 {
                        let data = json!({
                            "repo_id": event_info.id,
                            "org_id": event_info.org_id,
                            "org_slug": event_info.org_slug,
                            "slug": event_info.slug,
                            "pool": event_info.pool,
                            "clone_url": event_info.clone_url,
                        });
                        append(&tx, &source, "repo.created", data).await?;
                    }
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("create_repo"),
                    timeout: None,
                },
            )
            .await?;
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        Ok(info)
    }
}

impl Handler<ResolveRepo> for Org {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: ResolveRepo,
    ) -> Result<Option<RepoInfo>> {
        let org_slug = self.slug(&ctx).await?;
        Ok(self
            .repo_by_slug(&ctx, &org_slug, &action.slug)
            .await?
            .and_then(|(info, ready)| ready.then_some(info)))
    }
}

impl Handler<OrgStatusQuery> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: OrgStatusQuery) -> Result<OrgStatus> {
        Ok(OrgStatus {
            id: self.id.clone(),
            slug: self.slug(&ctx).await?,
            pool: ctx.inner().client_pool_name().map(str::to_owned),
        })
    }
}

impl Handler<Install> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Install) -> Result<InstallationInfo> {
        let slug = self.slug(&ctx).await?;
        let target = InstallTarget {
            scope: "org",
            org_id: self.id.clone(),
            repo_id: None,
            path: slug.to_string(),
            receives: RecipientScope::Org(self.id.clone()),
        };
        installations::install(&ctx, &self.outbox, &self.source(), target, action).await
    }
}

impl Handler<InstallationQuery> for Org {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: InstallationQuery,
    ) -> Result<Option<InstallationInfo>> {
        installations::ready(&ctx, &action.extension_id).await
    }
}

impl Handler<ListInstallations> for Org {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: ListInstallations,
    ) -> Result<Vec<InstallationInfo>> {
        installations::list(&ctx).await
    }
}

impl Handler<FlushTree> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: FlushTree) -> Result<()> {
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        let client = ctx.client()?;
        let repos = ctx
            .sql()
            .query("SELECT id, pool FROM repos WHERE ready = 1", None)
            .await?;
        for row in &repos.rows {
            let (id, pool) = (text(row, 0)?, optional_text(row, 1)?);
            if let Err(error) = repo_handle(&client, &id, pool)?.send(FlushTree {}).await {
                tracing::warn!(repo = %id, %error, "could not flush a repository");
            }
        }
        Ok(())
    }
}

impl Handler<ReadEvents> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ReadEvents) -> Result<Vec<Envelope>> {
        read(ctx.sql(), &self.source(), action.after, action.limit).await
    }
}

impl Handler<Flush> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Flush) -> Result<()> {
        self.outbox.flush(&ctx, SubscriberSource::Forge).await
    }
}
