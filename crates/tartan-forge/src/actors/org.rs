//! The org actor: one per org. It owns the org's repository directory.

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use rivetkit::prelude::*;
use rivetkit::{Handler, action};
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::collections::BTreeMap;

use crate::actors::installations::{
    self, INSTALLATIONS_SCHEMA, InstallTarget, InstallationInfo, InstallationQuery,
    ListInstallations, Wanted,
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
    // One reconciliation at a time.
    reconciling: tokio::sync::Mutex<()>,
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
    CREATE TABLE IF NOT EXISTS org_config (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        sha TEXT NOT NULL,
        config TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS contributions (
        repo_id TEXT NOT NULL,
        extension_id TEXT NOT NULL,
        path TEXT NOT NULL,
        settings TEXT NOT NULL,
        PRIMARY KEY (repo_id, extension_id)
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

/// Applies the configuration of the org's `tartan` repository at commit `sha`: it installs
/// the org's extensions. Sent by that repository's actor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyOrgConfig {
    /// The commit.
    pub sha: String,
    /// The evaluated configuration.
    pub config: tartan_config::Config,
}

impl Action for ApplyOrgConfig {
    type Output = ();

    const NAME: &'static str = "applyOrgConfig";
}

/// Replaces what a repository contributes to the org's installations: extension id to settings.
/// Sent by the repository's actor after it applies its configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetContributions {
    /// The repository.
    pub repo_id: String,
    /// Its `org/repo` path.
    pub path: String,
    /// Its settings for each org extension it configures.
    pub contributions: BTreeMap<String, serde_json::Value>,
}

impl Action for SetContributions {
    type Output = ();

    const NAME: &'static str = "setContributions";
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
        ApplyOrgConfig,
        SetContributions,
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
        Ok(Self {
            outbox,
            id,
            reconciling: tokio::sync::Mutex::new(()),
        })
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

impl Org {
    /// Applies the stored org configuration with the current contributions.
    async fn reconcile(&self, ctx: &Ctx<Self>) -> Result<()> {
        let _reconciling = self.reconciling.lock().await;
        let stored = ctx
            .sql()
            .query("SELECT sha, config FROM org_config WHERE id = 1", None)
            .await?;
        let Some(row) = stored.rows.first() else {
            return Ok(());
        };
        let sha = text(row, 0)?;
        let config: tartan_config::Config = serde_json::from_str(&text(row, 1)?)?;
        let without_version: Vec<String> = config
            .extensions
            .iter()
            .filter(|(_, entry)| entry.version.is_none())
            .map(|(id, _)| format!("{id} has no version: the org's configuration installs"))
            .collect();
        if !without_version.is_empty() {
            return installations::refuse(ctx, &self.outbox, &self.source(), &sha, without_version)
                .await;
        }
        let contributed = ctx
            .sql()
            .query(
                "SELECT repo_id, extension_id, path, settings FROM contributions
                 ORDER BY repo_id",
                None,
            )
            .await?;
        let mut repos: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for row in &contributed.rows {
            let (repo_id, extension_id) = (text(row, 0)?, text(row, 1)?);
            let settings: serde_json::Value = serde_json::from_str(&text(row, 3)?)?;
            repos.entry(extension_id).or_default().insert(
                repo_id,
                json!({ "path": text(row, 2)?, "settings": settings }),
            );
        }
        let wanted = config
            .extensions
            .into_iter()
            .filter_map(|(extension_id, entry)| {
                let version = entry.version?;
                let mut config = match entry.settings {
                    serde_json::Value::Object(map) => map,
                    _ => serde_json::Map::new(),
                };
                let contributions = repos.remove(&extension_id).unwrap_or_default();
                config.insert("repos".to_owned(), serde_json::Value::Object(contributions));
                Some((
                    extension_id,
                    Wanted {
                        version,
                        config: serde_json::Value::Object(config),
                    },
                ))
            })
            .collect();
        let target = InstallTarget {
            scope: "org",
            org_id: self.id.clone(),
            repo_id: None,
            path: self.slug(ctx).await?.to_string(),
            receives: RecipientScope::Org(self.id.clone()),
        };
        installations::reconcile(ctx, &self.outbox, &self.source(), &target, &sha, wanted).await?;
        Ok(())
    }
}

impl Handler<ApplyOrgConfig> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ApplyOrgConfig) -> Result<()> {
        ctx.sql()
            .execute(
                "INSERT INTO org_config (id, sha, config) VALUES (1, ?, ?)
                 ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, config = excluded.config",
                Some(vec![
                    param_text(action.sha.as_str()),
                    param_text(serde_json::to_string(&action.config)?),
                ]),
            )
            .await?;
        self.reconcile(&ctx).await
    }
}

impl Handler<SetContributions> for Org {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: SetContributions) -> Result<()> {
        let mut changed = false;
        let existing = ctx
            .sql()
            .query(
                "SELECT extension_id, path, settings FROM contributions WHERE repo_id = ?",
                Some(vec![param_text(action.repo_id.as_str())]),
            )
            .await?;
        let mut current = BTreeMap::new();
        for row in &existing.rows {
            current.insert(text(row, 0)?, (text(row, 1)?, text(row, 2)?));
        }
        for extension_id in current.keys() {
            if !action.contributions.contains_key(extension_id) {
                ctx.sql()
                    .execute(
                        "DELETE FROM contributions WHERE repo_id = ? AND extension_id = ?",
                        Some(vec![
                            param_text(action.repo_id.as_str()),
                            param_text(extension_id.as_str()),
                        ]),
                    )
                    .await?;
                changed = true;
            }
        }
        for (extension_id, settings) in &action.contributions {
            let settings = settings.to_string();
            if current.get(extension_id) == Some(&(action.path.clone(), settings.clone())) {
                continue;
            }
            ctx.sql()
                .execute(
                    "INSERT INTO contributions (repo_id, extension_id, path, settings)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT (repo_id, extension_id) DO UPDATE SET path = excluded.path,
                       settings = excluded.settings",
                    Some(vec![
                        param_text(action.repo_id.as_str()),
                        param_text(extension_id.as_str()),
                        param_text(action.path.as_str()),
                        param_text(settings),
                    ]),
                )
                .await?;
            changed = true;
        }
        if changed {
            self.reconcile(&ctx).await?;
        }
        Ok(())
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
