//! The repository actor: one per repository. Git storage belongs to the [`GitHost`]; this
//! actor owns the repository's forge state and turns the host's ref events into forge events.
//!
//! [`GitHost`]: crate::GitHost

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
use crate::actors::org::{ApplyOrgConfig, SetContributions};
use crate::actors::{apply_schema, key_id};
use crate::event_log::{EVENT_LOG_SCHEMA, ReadEvents, append, read};
use crate::outbox::{Flush, FlushTree, Outbox, SubscriberSource};
use crate::services::services;
use crate::sql::{param_integer, param_text, text};
use crate::{Envelope, RecipientScope, RefEvent, Slug};
use rivetkit::TypedClientExt;

/// The repository actor.
#[derive(Debug)]
pub struct Repo {
    id: String,
    outbox: Arc<Outbox>,
    // One reconciliation at a time; each applies the latest recorded `main`.
    reconciling: tokio::sync::Mutex<()>,
}

/// The branch whose `tartan.cue` configures the repository.
const CONFIG_BRANCH: &str = "refs/heads/main";

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS repo (
        id TEXT PRIMARY KEY,
        org_id TEXT NOT NULL,
        org_slug TEXT NOT NULL,
        slug TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS host_pushes (
        seq INTEGER PRIMARY KEY
    );
    CREATE TABLE IF NOT EXISTS host_ref_updates (
        seq INTEGER NOT NULL,
        ref_name TEXT NOT NULL,
        PRIMARY KEY (seq, ref_name)
    );
    CREATE TABLE IF NOT EXISTS config_head (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        sha TEXT NOT NULL,
        applied INTEGER NOT NULL DEFAULT 0
    );
";

/// Where a repository actor runs, and what it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStatus {
    /// The repository actor's id.
    pub id: String,
    /// The org actor's id.
    pub org_id: String,
    /// The org's slug.
    pub org_slug: Slug,
    /// The repository's slug.
    pub slug: Slug,
    /// The Rivet pool hosting the actor.
    pub pool: Option<String>,
}

/// Creates the repository on the Git host and records its identity. Sent by its org actor;
/// repeating it changes nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Init {
    /// The repository actor's id, which is also its key.
    pub id: String,
    /// The org actor's id.
    pub org_id: String,
    /// The org's slug.
    pub org_slug: Slug,
    /// The repository's slug.
    pub slug: Slug,
}

impl Action for Init {
    type Output = ();

    const NAME: &'static str = "init";
}

/// Records ref events from the Git host. Events already recorded are skipped, so the host may
/// deliver them more than once. Returns how many forge events were appended.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordRefEvents {
    /// The host's events for this repository, in its log order.
    pub events: Vec<RefEvent>,
}

impl Action for RecordRefEvents {
    type Output = u32;

    const NAME: &'static str = "recordRefEvents";
}

/// What and where the repository actor is.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepoStatusQuery {}

impl Action for RepoStatusQuery {
    type Output = RepoStatus;

    const NAME: &'static str = "status";
}

#[async_trait]
impl Actor for Repo {
    type Input = ();
    type Actions = (
        Init,
        RecordRefEvents,
        RepoStatusQuery,
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
        let outbox = Outbox::new(format!("repo:{id}"));
        let org = ctx.sql().query("SELECT org_id FROM repo", None).await?;
        if let Some(row) = org.rows.first() {
            outbox.set_org(text(row, 0)?);
        }
        Ok(Self {
            outbox,
            id,
            reconciling: tokio::sync::Mutex::new(()),
        })
    }

    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        // A configuration recorded but not applied before the actor stopped is applied now.
        self.reconcile_later(&ctx);
        Ok(())
    }
}

impl Repo {
    fn source(&self) -> String {
        format!("repo:{}", self.id)
    }

    /// The repository's `org/slug` path, or an error before [`Init`].
    async fn path(&self, ctx: &Ctx<Self>) -> Result<(Slug, Slug)> {
        let result = ctx
            .sql()
            .query("SELECT org_slug, slug FROM repo", None)
            .await?;
        match result.rows.first() {
            Some(row) => Ok((text(row, 0)?.parse()?, text(row, 1)?.parse()?)),
            None => bail!("repository {} has not been initialized", self.id),
        }
    }
}

impl Handler<Init> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Init) -> Result<()> {
        if action.id != self.id {
            bail!("init for repository {} sent to {}", action.id, self.id);
        }
        let host = &services()?.git_host;
        host.create_repo(action.org_slug.as_str(), action.slug.as_str())
            .await?;
        let clone_url = host.clone_url(action.org_slug.as_str(), action.slug.as_str());
        self.outbox.set_org(action.org_id.clone());
        let source = self.source();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let inserted = tx
                        .execute(
                            "INSERT OR IGNORE INTO repo (id, org_id, org_slug, slug)
                             VALUES (?, ?, ?, ?)",
                            Some(vec![
                                param_text(action.id.as_str()),
                                param_text(action.org_id.as_str()),
                                param_text(action.org_slug.as_str()),
                                param_text(action.slug.as_str()),
                            ]),
                        )
                        .await?;
                    if inserted.changes == 1 {
                        let data = json!({
                            "repo_id": action.id,
                            "path": format!("{}/{}", action.org_slug, action.slug),
                            "clone_url": clone_url,
                        });
                        append(&tx, &source, "repo.ready", data).await?;
                    }
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("init_repo"),
                    timeout: None,
                },
            )
            .await?;
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        Ok(())
    }
}

impl Handler<RecordRefEvents> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: RecordRefEvents) -> Result<u32> {
        let (org_slug, slug) = self.path(&ctx).await?;
        let path = format!("{org_slug}/{slug}");
        let mut events = Vec::with_capacity(action.events.len());
        for event in action.events {
            if event.repo != path {
                bail!("ref event for {} sent to repository {path}", event.repo);
            }
            let seq: u64 = event.walgit.seq.parse()?;
            events.push((seq, event));
        }
        // The host's log order; a stable sort keeps the order of updates within one push.
        events.sort_by_key(|(seq, _)| *seq);

        let source = self.source();
        let (appended, config_moved) = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    let mut appended = 0_u32;
                    let mut config_head = None;
                    for (seq, event) in events {
                        let seq_param = param_integer(i64::try_from(seq)?);
                        let new_push = tx
                            .execute(
                                "INSERT OR IGNORE INTO host_pushes (seq) VALUES (?)",
                                Some(vec![seq_param.clone()]),
                            )
                            .await?;
                        if new_push.changes == 1 {
                            let data = json!({
                                "host_seq": seq,
                                "pusher": event.pusher,
                                "entry_kind": event.walgit.entry_kind,
                            });
                            append(&tx, &source, "push.accepted", data).await?;
                            appended += 1;
                        }
                        let new_update = tx
                            .execute(
                                "INSERT OR IGNORE INTO host_ref_updates (seq, ref_name)
                                 VALUES (?, ?)",
                                Some(vec![seq_param, param_text(event.ref_name.as_str())]),
                            )
                            .await?;
                        if new_update.changes == 1 {
                            let data = json!({
                                "host_seq": seq,
                                "action": event.action,
                                "ref_type": event.ref_type,
                                "ref_name": event.ref_name,
                                "old": event.old,
                                "new": event.new,
                                "pusher": event.pusher,
                            });
                            append(&tx, &source, "ref.updated", data).await?;
                            appended += 1;
                            let moved = event.ref_name == CONFIG_BRANCH
                                && !event.new.bytes().all(|b| b == b'0');
                            if moved {
                                config_head = Some(event.new.clone());
                            }
                        }
                    }
                    if let Some(sha) = &config_head {
                        tx.execute(
                            "INSERT INTO config_head (id, sha, applied) VALUES (1, ?, 0)
                             ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, applied = 0",
                            Some(vec![param_text(sha.as_str())]),
                        )
                        .await?;
                    }
                    Ok((appended, config_head.is_some()))
                },
                SqliteTransactionOptions {
                    name: Some("record_ref_events"),
                    timeout: None,
                },
            )
            .await?;
        if appended > 0 {
            self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        }
        if config_moved {
            self.reconcile_later(&ctx);
        }
        Ok(appended)
    }
}

/// The repository at one commit, read through the Git host.
struct HostSource {
    owner: String,
    name: String,
    rev: String,
}

#[async_trait]
impl tartan_config::ConfigSource for HostSource {
    async fn list(&self, dir: &str) -> Result<Option<Vec<tartan_config::DirEntry>>> {
        services()?
            .git_host
            .list_dir(&self.owner, &self.name, &self.rev, dir)
            .await
    }

    async fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        services()?
            .git_host
            .read_file(&self.owner, &self.name, &self.rev, path)
            .await
    }
}

impl Handler<RepoStatusQuery> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: RepoStatusQuery,
    ) -> Result<RepoStatus> {
        let (org_slug, slug) = self.path(&ctx).await?;
        Ok(RepoStatus {
            id: self.id.clone(),
            org_id: self.org_id(&ctx).await?,
            org_slug,
            slug,
            pool: ctx.inner().client_pool_name().map(str::to_owned),
        })
    }
}

impl Repo {
    /// Applies the latest recorded `main` in the background.
    fn reconcile_later(self: &Arc<Self>, ctx: &Ctx<Self>) {
        let repo = Arc::clone(self);
        let ctx = ctx.clone();
        ctx.clone().wait_until(async move {
            if let Err(error) = repo.reconcile(&ctx).await {
                tracing::warn!(repo = %repo.id, error = format!("{error:#}"),
                    "could not apply the repository's configuration");
            }
        });
    }

    /// Applies `tartan.cue` at the latest recorded `main`, unless it is applied already. The
    /// org's `tartan` repository configures the org instead of itself.
    async fn reconcile(&self, ctx: &Ctx<Self>) -> Result<()> {
        let _reconciling = self.reconciling.lock().await;
        let head = ctx
            .sql()
            .query("SELECT sha, applied FROM config_head WHERE id = 1", None)
            .await?;
        let Some(row) = head.rows.first() else {
            return Ok(());
        };
        let (sha, applied) = (text(row, 0)?, crate::sql::integer(row, 1)? != 0);
        if applied {
            return Ok(());
        }
        let (org_slug, slug) = self.path(ctx).await?;
        let org_id = self.org_id(ctx).await?;
        let source = HostSource {
            owner: org_slug.to_string(),
            name: slug.to_string(),
            rev: sha.clone(),
        };
        let evaluated = match tartan_config::load(&source, tartan_config::LIMITS).await {
            Ok(files) => tartan_config::evaluate(files)
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        let config = match evaluated {
            Ok(config) => Some(config),
            Err(problem) => {
                installations::refuse(ctx, &self.outbox, &self.source(), &sha, vec![problem])
                    .await?;
                None
            }
        };
        if let Some(config) = config {
            let org = ctx.client()?.get_typed::<crate::Org>(
                crate::actors::ORG,
                [org_id.as_str()],
                rivetkit::client::GetOptions::default(),
            )?;
            if slug.as_str() == tartan_config::ORG_CONFIG_REPO {
                org.send(ApplyOrgConfig {
                    sha: sha.clone(),
                    config,
                })
                .await?;
            } else {
                let mut wanted = BTreeMap::new();
                let mut contributions = BTreeMap::new();
                for (extension_id, entry) in config.extensions {
                    match entry.version {
                        Some(version) => {
                            wanted.insert(
                                extension_id,
                                Wanted {
                                    version,
                                    config: entry.settings,
                                },
                            );
                        }
                        None => {
                            contributions.insert(extension_id, entry.settings);
                        }
                    }
                }
                let target = InstallTarget {
                    scope: "repo",
                    org_id: org_id.clone(),
                    repo_id: Some(self.id.clone()),
                    path: format!("{org_slug}/{slug}"),
                    receives: RecipientScope::Source(self.source()),
                };
                let applied = installations::reconcile(
                    ctx,
                    &self.outbox,
                    &self.source(),
                    &target,
                    &sha,
                    wanted,
                )
                .await?;
                if applied {
                    org.send(SetContributions {
                        repo_id: self.id.clone(),
                        path: format!("{org_slug}/{slug}"),
                        contributions,
                    })
                    .await?;
                }
            }
        }
        ctx.sql()
            .execute(
                "UPDATE config_head SET applied = 1 WHERE id = 1 AND sha = ?",
                Some(vec![param_text(sha.as_str())]),
            )
            .await?;
        Ok(())
    }

    async fn org_id(&self, ctx: &Ctx<Self>) -> Result<String> {
        let result = ctx.sql().query("SELECT org_id FROM repo", None).await?;
        match result.rows.first() {
            Some(row) => text(row, 0),
            None => bail!("repository {} has not been initialized", self.id),
        }
    }
}

impl Handler<InstallationQuery> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: InstallationQuery,
    ) -> Result<Option<InstallationInfo>> {
        installations::ready(&ctx, &action.extension_id).await
    }
}

impl Handler<ListInstallations> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: ListInstallations,
    ) -> Result<Vec<InstallationInfo>> {
        installations::list(&ctx).await
    }
}

impl Handler<FlushTree> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: FlushTree) -> Result<()> {
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        let client = ctx.client()?;
        for installation in installations::list(&ctx).await? {
            let flushed = client
                .get_typed::<crate::Extension>(
                    crate::actors::EXTENSION,
                    [installation.installation_id.as_str()],
                    rivetkit::client::GetOptions::default(),
                )?
                .send(Flush {})
                .await;
            if let Err(error) = flushed {
                tracing::warn!(installation = %installation.installation_id, %error,
                    "could not flush an installation");
            }
        }
        Ok(())
    }
}

impl Handler<ReadEvents> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ReadEvents) -> Result<Vec<Envelope>> {
        read(ctx.sql(), &self.source(), action.after, action.limit).await
    }
}

impl Handler<Flush> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Flush) -> Result<()> {
        self.outbox.flush(&ctx, SubscriberSource::Forge).await
    }
}
