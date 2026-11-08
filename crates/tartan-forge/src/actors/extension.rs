//! The extension actor: one per installation. Its SQLite database is the installation's own;
//! the host keeps its tables there under the `_tartan_` prefix, which the extension cannot reach.

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use rivetkit::prelude::*;
use rivetkit::{BindParam, ColumnValue, Handler, SqliteDb, action};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tartan_extensions::{
    Actor as ExtensionActor, ActorKind, CallEnvironment, CallLimits, CallOutcome, Effect,
    ExtensionError, InstallMode, SlotContext, SqlBackend, SqlRows, SqlValue, apply_migrations,
};
use tokio::sync::Mutex;

use crate::Envelope;
use crate::actors::{apply_schema, key_id};
use crate::event_log::{EVENT_LOG_SCHEMA, ReadEvents, append, read};
use crate::extensions::Loaded;
use crate::outbox::{Flush, Outbox, SubscriberSource};
use crate::services::services;
use crate::sql::{param_text, text};

/// The extension actor.
pub struct Extension {
    id: String,
    outbox: Arc<Outbox>,
    // One call at a time: an extension's statements are not wrapped in a transaction, so calls
    // must not interleave.
    calls: Mutex<()>,
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS _tartan_installation (
        id TEXT PRIMARY KEY,
        extension_id TEXT NOT NULL,
        version TEXT NOT NULL,
        digest TEXT NOT NULL,
        config TEXT NOT NULL,
        repo_id TEXT NOT NULL,
        repo_path TEXT NOT NULL
    );
";

/// What an installation runs and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationSpec {
    /// The installation's id, which is also the actor's key.
    pub installation_id: String,
    /// The extension's id, such as `rawkode.issues`.
    pub extension_id: String,
    /// The package version.
    pub version: String,
    /// The package digest.
    pub digest: String,
    /// The installation's configuration, as JSON.
    pub config: String,
    /// The repository the installation belongs to.
    pub repo_id: String,
    /// The repository's `org/repo` path.
    pub repo_path: String,
}

/// The reply of an extension call: its JSON result, or the error it returned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallReply {
    /// The call's JSON result.
    Ok(Value),
    /// The extension refused or failed the call.
    Failed {
        /// `denied`, `not_found`, `invalid`, `conflict`, `unavailable` or `internal`.
        kind: String,
        /// The extension's message.
        message: String,
    },
}

/// Sets the installation up: applies the package's migrations and calls `init`. Repeating it
/// changes nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Init {
    /// What to install.
    pub spec: InstallationSpec,
}

impl Action for Init {
    type Output = ();

    const NAME: &'static str = "init";
}

/// Calls one of the extension's tools.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallTool {
    /// The tool.
    pub tool: String,
    /// Its JSON arguments.
    pub args: Value,
    /// The principal calling it.
    pub viewer: String,
}

impl Action for CallTool {
    type Output = CallReply;

    const NAME: &'static str = "callTool";
}

/// Renders one of the extension's slots; the extension may only read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Render {
    /// The slot contribution's id, such as `issues`.
    pub slot: String,
    /// Properties for the render.
    pub props: Value,
    /// The principal viewing it.
    pub viewer: String,
}

impl Action for Render {
    type Output = CallReply;

    const NAME: &'static str = "render";
}

/// Runs one of the extension's UI actions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Act {
    /// The action.
    pub action: String,
    /// Its JSON payload.
    pub payload: Value,
    /// The principal acting.
    pub viewer: String,
}

impl Action for Act {
    type Output = CallReply;

    const NAME: &'static str = "act";
}

/// A timer the extension set came due. Scheduled by the actor itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timer {
    /// The timer's key.
    pub key: String,
}

impl Action for Timer {
    type Output = ();

    const NAME: &'static str = "timer";
}

#[async_trait]
impl Actor for Extension {
    type Input = ();
    type Actions = (Init, CallTool, Render, Act, Timer, ReadEvents, Flush);
    type Events = ();
    type ConnParams = ();
    type Action = action::Raw;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        apply_schema(ctx.sql(), EVENT_LOG_SCHEMA).await?;
        apply_schema(ctx.sql(), SCHEMA).await?;
        let id = key_id(ctx.key())?;
        Ok(Self {
            outbox: Outbox::new(format!("extension:{id}")),
            id,
            calls: Mutex::new(()),
        })
    }

    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        Ok(())
    }
}

impl Extension {
    async fn spec(&self, ctx: &Ctx<Self>) -> Result<Option<InstallationSpec>> {
        let result = ctx
            .sql()
            .query(
                "SELECT extension_id, version, digest, config, repo_id, repo_path
                 FROM _tartan_installation",
                None,
            )
            .await?;
        let Some(row) = result.rows.first() else {
            return Ok(None);
        };
        Ok(Some(InstallationSpec {
            installation_id: self.id.clone(),
            extension_id: text(row, 0)?,
            version: text(row, 1)?,
            digest: text(row, 2)?,
            config: text(row, 3)?,
            repo_id: text(row, 4)?,
            repo_path: text(row, 5)?,
        }))
    }

    async fn installed(&self, ctx: &Ctx<Self>) -> Result<(InstallationSpec, Loaded)> {
        let Some(spec) = self.spec(ctx).await? else {
            bail!("installation {} has not been initialized", self.id);
        };
        let loaded = services()?
            .extensions
            .load(&spec.extension_id, &spec.version, &spec.digest)
            .await?;
        Ok((spec, loaded))
    }

    fn environment(&self, ctx: &Ctx<Self>, spec: &InstallationSpec) -> CallEnvironment {
        CallEnvironment {
            sql: Arc::new(ActorSql(ctx.sql().clone())),
            config: spec.config.clone(),
            limits: CallLimits::default(),
            label: format!("{}@{}", spec.extension_id, self.id),
        }
    }

    /// Applies a successful call's effects and turns its result into a reply.
    async fn reply(
        &self,
        ctx: &Ctx<Self>,
        spec: &InstallationSpec,
        outcome: CallOutcome<String>,
    ) -> Result<CallReply> {
        self.apply_effects(ctx, spec, outcome.effects).await?;
        Ok(match outcome.result {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(value) => CallReply::Ok(value),
                Err(_) => CallReply::Failed {
                    kind: "internal".to_owned(),
                    message: "the extension returned a result that is not JSON".to_owned(),
                },
            },
            Err(error) => failed(error),
        })
    }

    async fn apply_effects(
        &self,
        ctx: &Ctx<Self>,
        spec: &InstallationSpec,
        effects: Vec<Effect>,
    ) -> Result<()> {
        let mut emitted = Vec::new();
        for effect in effects {
            match effect {
                Effect::Emit {
                    kind,
                    subject,
                    data,
                } => {
                    if !valid_event_kind(&kind) {
                        tracing::warn!(installation = %self.id, %kind,
                            "dropped an emitted event with an invalid type");
                        continue;
                    }
                    let subject = subject.map(|s| json!({ "kind": s.kind, "id": s.id }));
                    emitted.push((
                        format!("x.{}.{kind}", spec.extension_id),
                        json!({
                            "installation_id": self.id,
                            "repo_id": spec.repo_id,
                            "subject": subject,
                            "data": serde_json::from_str::<Value>(&data)?,
                        }),
                    ));
                }
                Effect::SetTimer { key, at_ms } => {
                    let args = rivetkit::action::encode_positional(&Timer { key })?;
                    ctx.schedule()
                        .at(i64::try_from(at_ms)?, Timer::NAME, &args)
                        .await?;
                }
            }
        }
        if emitted.is_empty() {
            return Ok(());
        }
        let source = self.outbox_source();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    for (kind, data) in emitted {
                        append(&tx, &source, &kind, data).await?;
                    }
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("emit"),
                    timeout: None,
                },
            )
            .await?;
        self.outbox.flush_later(ctx, SubscriberSource::Forge);
        Ok(())
    }

    fn outbox_source(&self) -> String {
        format!("extension:{}", self.id)
    }
}

impl Handler<Init> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Init) -> Result<()> {
        let spec = action.spec;
        if spec.installation_id != self.id {
            bail!(
                "init for installation {} sent to {}",
                spec.installation_id,
                self.id
            );
        }
        let _call = self.calls.lock().await;
        if self.spec(&ctx).await?.is_some() {
            return Ok(());
        }
        let loaded = services()?
            .extensions
            .load(&spec.extension_id, &spec.version, &spec.digest)
            .await?;
        let sql = ActorSql(ctx.sql().clone());
        apply_migrations(&sql, &loaded.package.migrations).await?;
        let outcome = loaded.component.init(self.environment(&ctx, &spec)).await?;
        if let Err(error) = outcome.result {
            let CallReply::Failed { kind, message } = failed(error) else {
                unreachable!("failed() returns Failed");
            };
            bail!("{} init failed ({kind}): {message}", spec.extension_id);
        }
        self.apply_effects(&ctx, &spec, outcome.effects).await?;
        ctx.sql()
            .execute(
                "INSERT INTO _tartan_installation
                 (id, extension_id, version, digest, config, repo_id, repo_path)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                Some(vec![
                    param_text(spec.installation_id.as_str()),
                    param_text(spec.extension_id.as_str()),
                    param_text(spec.version.as_str()),
                    param_text(spec.digest.as_str()),
                    param_text(spec.config.as_str()),
                    param_text(spec.repo_id.as_str()),
                    param_text(spec.repo_path.as_str()),
                ]),
            )
            .await?;
        Ok(())
    }
}

impl Handler<CallTool> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: CallTool) -> Result<CallReply> {
        let _call = self.calls.lock().await;
        let (spec, loaded) = self.installed(&ctx).await?;
        let context = slot_context(&spec, None, &action.viewer);
        let outcome = loaded
            .component
            .call_tool(
                self.environment(&ctx, &spec),
                &action.tool,
                &action.args.to_string(),
                &context,
            )
            .await?;
        self.reply(&ctx, &spec, outcome).await
    }
}

impl Handler<Render> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Render) -> Result<CallReply> {
        let _call = self.calls.lock().await;
        let (spec, loaded) = self.installed(&ctx).await?;
        let context = slot_context(&spec, Some(&action.slot), &action.viewer);
        let outcome = loaded
            .component
            .render(
                self.environment(&ctx, &spec),
                &action.slot,
                &context,
                &action.props.to_string(),
            )
            .await?;
        self.reply(&ctx, &spec, outcome).await
    }
}

impl Handler<Act> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Act) -> Result<CallReply> {
        let _call = self.calls.lock().await;
        let (spec, loaded) = self.installed(&ctx).await?;
        let context = slot_context(&spec, None, &action.viewer);
        let outcome = loaded
            .component
            .on_action(
                self.environment(&ctx, &spec),
                &action.action,
                &action.payload.to_string(),
                &context,
            )
            .await?;
        self.reply(&ctx, &spec, outcome).await
    }
}

impl Handler<Timer> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Timer) -> Result<()> {
        let _call = self.calls.lock().await;
        let (spec, loaded) = self.installed(&ctx).await?;
        let outcome = loaded
            .component
            .on_timer(self.environment(&ctx, &spec), &action.key)
            .await?;
        if let Err(error) = &outcome.result {
            tracing::warn!(installation = %self.id, key = %action.key, ?error, "on-timer failed");
        }
        self.apply_effects(&ctx, &spec, outcome.effects).await
    }
}

impl Handler<ReadEvents> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ReadEvents) -> Result<Vec<Envelope>> {
        read(ctx.sql(), &self.outbox_source(), action.after, action.limit).await
    }
}

impl Handler<Flush> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Flush) -> Result<()> {
        self.outbox.flush(&ctx, SubscriberSource::Forge).await
    }
}

/// The context of a call: the installation's repository and the principal calling.
fn slot_context(spec: &InstallationSpec, slot: Option<&str>, viewer: &str) -> SlotContext {
    SlotContext {
        slot: slot.map(str::to_owned),
        node: spec.repo_id.clone(),
        repo: Some(spec.repo_id.clone()),
        git_ref: None,
        path: None,
        entity: None,
        viewer: Some(ExtensionActor {
            kind: ActorKind::User,
            id: viewer.to_owned(),
            on_behalf_of: None,
        }),
        mode: InstallMode::Enforce,
        extra: None,
    }
}

fn failed(error: ExtensionError) -> CallReply {
    let (kind, message) = match error {
        ExtensionError::Denied(m) => ("denied", m),
        ExtensionError::NotFound(m) => ("not_found", m),
        ExtensionError::Invalid(m) => ("invalid", m),
        ExtensionError::Conflict(m) => ("conflict", m),
        ExtensionError::Unavailable(m) => ("unavailable", m),
        ExtensionError::Internal(m) => ("internal", m),
    };
    CallReply::Failed {
        kind: kind.to_owned(),
        message,
    }
}

/// Whether an extension-emitted event type is dot-separated lowercase tokens.
fn valid_event_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 100
        && kind.split('.').all(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// The installation's database, as the extension host sees it.
struct ActorSql(SqliteDb);

#[async_trait]
impl SqlBackend for ActorSql {
    async fn exec(&self, query: &str, params: Vec<SqlValue>) -> Result<SqlRows> {
        let params = params
            .into_iter()
            .map(|value| match value {
                SqlValue::Null => BindParam::Null,
                SqlValue::Integer(v) => BindParam::Integer(v),
                SqlValue::Real(v) => BindParam::Float(v),
                SqlValue::Text(v) => BindParam::Text(v),
                SqlValue::Blob(v) => BindParam::Blob(v),
            })
            .collect();
        let result = self.0.execute(query, Some(params)).await?;
        // SQLite's change count is the last write's, so a query reports none of its own.
        let leading = query
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let reads_only = matches!(leading.as_str(), "select" | "with");
        Ok(SqlRows {
            columns: result.columns,
            values: result
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(from_column).collect())
                .collect(),
            rows_written: if reads_only {
                0
            } else {
                u64::try_from(result.changes).unwrap_or_default()
            },
        })
    }

    async fn exec_script(&self, script: &str) -> Result<()> {
        self.0.exec(script).await?;
        Ok(())
    }
}

fn from_column(value: ColumnValue) -> SqlValue {
    match value {
        ColumnValue::Null => SqlValue::Null,
        ColumnValue::Integer(v) => SqlValue::Integer(v),
        ColumnValue::Float(v) => SqlValue::Real(v),
        ColumnValue::Text(v) => SqlValue::Text(v),
        ColumnValue::Blob(v) => SqlValue::Blob(v),
    }
}

#[cfg(test)]
mod tests {
    use super::valid_event_kind;

    #[test]
    fn emitted_event_types() {
        assert!(valid_event_kind("issue.created"));
        assert!(!valid_event_kind("Issue.created"));
        assert!(!valid_event_kind("issue..created"));
        assert!(!valid_event_kind(""));
    }
}
