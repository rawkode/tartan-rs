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
    ExtensionError, InstallMode, SlotContext, SqlBackend, SqlRows, SqlValue, ToolCaller,
    apply_migrations,
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
        org_id TEXT NOT NULL,
        repo_id TEXT NOT NULL,
        path TEXT NOT NULL,
        disabled INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS _tartan_cursors (
        source TEXT PRIMARY KEY,
        seq INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS _tartan_dead_letters (
        source TEXT NOT NULL,
        seq INTEGER NOT NULL,
        type TEXT NOT NULL,
        reason TEXT NOT NULL,
        at_ms INTEGER NOT NULL,
        PRIMARY KEY (source, seq)
    );
";

/// What an installation runs and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationSpec {
    /// The installation's id, which is also the actor's key.
    pub installation_id: String,
    /// The extension's id, such as `issues.tartan.rawkode.academy`.
    pub extension_id: String,
    /// The package version.
    pub version: String,
    /// The package digest.
    pub digest: String,
    /// The installation's configuration, as JSON.
    pub config: String,
    /// The org the repository belongs to.
    pub org_id: String,
    /// The repository the installation is on.
    pub repo_id: String,
    /// The repository's `org/repo` path.
    pub path: String,
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

/// Applies an installation's spec: installs it (migrations, then `init`), upgrades it to a new
/// package (the new migrations, then `init` again) or changes its configuration, and enables it
/// if it was disabled. Repeating it changes nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Init {
    /// What to install.
    pub spec: InstallationSpec,
}

impl Action for Init {
    type Output = ();

    const NAME: &'static str = "init";
}

/// Disables the installation: its configuration removed it. Its data is kept, so configuring it
/// again restores it; until then every call is refused.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Disable {}

impl Action for Disable {
    type Output = ();

    const NAME: &'static str = "disable";
}

/// Who a call acts for (spec: extension-runtime): a person, or an agent acting on behalf of
/// one. The gateway builds it from the request's principal; the extension sees it as the slot
/// context's `viewer`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    /// A person, or an agent acting for one.
    pub kind: CallerKind,
    /// The person's DID, or `token:<id>` for an access token.
    pub id: String,
    /// The DID an agent acts for.
    pub on_behalf_of: Option<String>,
}

/// The kinds of [`Caller`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerKind {
    /// A signed-in person.
    User,
    /// An access token acting for a person.
    Agent,
    /// The installation itself, for events and timers.
    Ext,
}

impl Caller {
    /// A signed-in person.
    pub fn user(did: impl Into<String>) -> Self {
        Self {
            kind: CallerKind::User,
            id: did.into(),
            on_behalf_of: None,
        }
    }

    /// An access token acting for `did`.
    pub fn agent(token_id: &str, did: impl Into<String>) -> Self {
        Self {
            kind: CallerKind::Agent,
            id: format!("token:{token_id}"),
            on_behalf_of: Some(did.into()),
        }
    }

    /// The caller as the extension world describes it.
    fn to_extension(&self) -> ExtensionActor {
        ExtensionActor {
            kind: match self.kind {
                CallerKind::User => ActorKind::User,
                CallerKind::Agent => ActorKind::Agent,
                CallerKind::Ext => ActorKind::Ext,
            },
            id: self.id.clone(),
            on_behalf_of: self.on_behalf_of.clone(),
        }
    }
}

/// Calls one of the extension's tools.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallTool {
    /// The tool.
    pub tool: String,
    /// Its JSON arguments.
    pub args: Value,
    /// The principal calling it.
    pub viewer: Caller,
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
    pub viewer: Caller,
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
    pub viewer: Caller,
}

impl Action for Act {
    type Output = CallReply;

    const NAME: &'static str = "act";
}

/// Events for the installation's `on-event`, in order, from one source. Each event reaches
/// `on-event` once; one the extension fails is kept as a dead letter and the rest continue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliverEvents {
    /// The source.
    pub source: String,
    /// The events, in order.
    pub events: Vec<Envelope>,
}

impl Action for DeliverEvents {
    type Output = ();

    const NAME: &'static str = "deliverEvents";
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
    type Actions = (
        Init,
        Disable,
        CallTool,
        Render,
        Act,
        DeliverEvents,
        Timer,
        ReadEvents,
        Flush,
    );
    type Events = ();
    type ConnParams = ();
    type Action = action::Raw;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        apply_schema(ctx.sql(), EVENT_LOG_SCHEMA).await?;
        apply_schema(ctx.sql(), SCHEMA).await?;
        let id = key_id(ctx.key())?;
        let outbox = Outbox::new(format!("extension:{id}"));
        let repo = ctx
            .sql()
            .query("SELECT repo_id FROM _tartan_installation", None)
            .await?;
        if let Some(row) = repo.rows.first() {
            outbox.set_repo(text(row, 0)?);
        }
        Ok(Self {
            outbox,
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
                "SELECT extension_id, version, digest, config, org_id, repo_id, path
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
            org_id: text(row, 4)?,
            repo_id: text(row, 5)?,
            path: text(row, 6)?,
        }))
    }

    async fn disabled(&self, ctx: &Ctx<Self>) -> Result<bool> {
        let result = ctx
            .sql()
            .query("SELECT disabled FROM _tartan_installation", None)
            .await?;
        match result.rows.first() {
            Some(row) => Ok(crate::sql::integer(row, 0)? != 0),
            None => Ok(false),
        }
    }

    async fn installed(&self, ctx: &Ctx<Self>) -> Result<(InstallationSpec, Loaded)> {
        let Some(spec) = self.spec(ctx).await? else {
            bail!("installation {} has not been initialized", self.id);
        };
        if self.disabled(ctx).await? {
            bail!(
                "{} is not installed: its configuration removed it",
                spec.extension_id
            );
        }
        let loaded = services()?
            .extensions
            .load(&spec.extension_id, &spec.version, &spec.digest)
            .await?;
        Ok((spec, loaded))
    }

    /// The environment of a call acting for `viewer`.
    fn environment(
        &self,
        ctx: &Ctx<Self>,
        spec: &InstallationSpec,
        loaded: &Loaded,
        viewer: &Caller,
    ) -> CallEnvironment {
        let caller = ctx.client().ok().map(|client| {
            Arc::new(InstallationCaller {
                client,
                repo_id: spec.repo_id.clone(),
                allowed: loaded.package.manifest.raw["permissions"]["calls"].clone(),
                viewer: viewer.clone(),
            }) as Arc<dyn ToolCaller>
        });
        CallEnvironment {
            sql: Arc::new(ActorSql(ctx.sql().clone())),
            config: spec.config.clone(),
            limits: CallLimits::default(),
            label: format!("{}@{}", spec.extension_id, self.id),
            caller,
        }
    }

    /// The principal an installation acts as when no person does: for events and timers.
    fn own_principal(&self) -> Caller {
        Caller {
            kind: CallerKind::Ext,
            id: format!("extension:{}", self.id),
            on_behalf_of: None,
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
                            "org_id": spec.org_id,
                            "repo_id": spec.repo_id,
                            "path": spec.path,
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
        let current = self.spec(&ctx).await?;
        let disabled = self.disabled(&ctx).await?;
        if current.as_ref() == Some(&spec) && !disabled {
            return Ok(());
        }
        let loaded = services()?
            .extensions
            .load(&spec.extension_id, &spec.version, &spec.digest)
            .await?;
        self.outbox.set_repo(spec.repo_id.clone());
        // A new package (or none yet) runs its migrations and `init`; a configuration change
        // alone does not.
        if current.as_ref().map(|c| c.digest.as_str()) != Some(spec.digest.as_str()) {
            let sql = ActorSql(ctx.sql().clone());
            apply_migrations(&sql, &loaded.package.migrations).await?;
            let principal = self.own_principal();
            let outcome = loaded
                .component
                .init(self.environment(&ctx, &spec, &loaded, &principal))
                .await?;
            if let Err(error) = outcome.result {
                let CallReply::Failed { kind, message } = failed(error) else {
                    unreachable!("failed() returns Failed");
                };
                bail!("{} init failed ({kind}): {message}", spec.extension_id);
            }
            self.apply_effects(&ctx, &spec, outcome.effects).await?;
        }
        ctx.sql()
            .execute(
                "INSERT OR REPLACE INTO _tartan_installation
                 (id, extension_id, version, digest, config, org_id, repo_id, path, disabled)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)",
                Some(vec![
                    param_text(spec.installation_id.as_str()),
                    param_text(spec.extension_id.as_str()),
                    param_text(spec.version.as_str()),
                    param_text(spec.digest.as_str()),
                    param_text(spec.config.as_str()),
                    param_text(spec.org_id.as_str()),
                    param_text(spec.repo_id.as_str()),
                    param_text(spec.path.as_str()),
                ]),
            )
            .await?;
        Ok(())
    }
}

impl Handler<Disable> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Disable) -> Result<()> {
        let _call = self.calls.lock().await;
        ctx.sql()
            .execute("UPDATE _tartan_installation SET disabled = 1", None)
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
                self.environment(&ctx, &spec, &loaded, &action.viewer),
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
                self.environment(&ctx, &spec, &loaded, &action.viewer),
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
                self.environment(&ctx, &spec, &loaded, &action.viewer),
                &action.action,
                &action.payload.to_string(),
                &context,
            )
            .await?;
        self.reply(&ctx, &spec, outcome).await
    }
}

impl Handler<DeliverEvents> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: DeliverEvents) -> Result<()> {
        // An installation never receives its own events, so an extension that emits what it
        // subscribes to cannot feed itself.
        if action.source == self.outbox_source() {
            return Ok(());
        }
        let _call = self.calls.lock().await;
        let (spec, loaded) = self.installed(&ctx).await?;
        let mut cursor = event_cursor(&ctx, &action.source).await?;
        for envelope in action.events {
            if envelope.seq <= cursor {
                continue;
            }
            let event = to_extension_event(&envelope, &spec);
            let failure = match loaded
                .component
                .on_event(
                    self.environment(&ctx, &spec, &loaded, &self.own_principal()),
                    &event,
                )
                .await
            {
                Ok(outcome) => match outcome.result {
                    Ok(()) => {
                        self.apply_effects(&ctx, &spec, outcome.effects).await?;
                        None
                    }
                    Err(error) => match failed(error) {
                        CallReply::Failed { kind, message } => Some(format!("{kind}: {message}")),
                        CallReply::Ok(_) => None,
                    },
                },
                // A trap, an exhausted limit or a timeout: the same event would fail again.
                Err(error) => Some(format!("{error:#}")),
            };
            if let Some(reason) = failure {
                tracing::warn!(installation = %self.id, source = %envelope.source,
                    seq = envelope.seq, %reason, "on-event failed; kept as a dead letter");
                ctx.sql()
                    .execute(
                        "INSERT OR REPLACE INTO _tartan_dead_letters
                         (source, seq, type, reason, at_ms) VALUES (?, ?, ?, ?, ?)",
                        Some(vec![
                            param_text(envelope.source.as_str()),
                            BindParam::Integer(i64::try_from(envelope.seq)?),
                            param_text(envelope.kind.as_str()),
                            param_text(reason),
                            BindParam::Integer(crate::sql::now_ms()),
                        ]),
                    )
                    .await?;
            }
            cursor = envelope.seq;
            ctx.sql()
                .execute(
                    "INSERT INTO _tartan_cursors (source, seq) VALUES (?, ?)
                     ON CONFLICT (source) DO UPDATE SET seq = MAX(seq, excluded.seq)",
                    Some(vec![
                        param_text(action.source.as_str()),
                        BindParam::Integer(i64::try_from(cursor)?),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
}

impl Handler<Timer> for Extension {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Timer) -> Result<()> {
        let _call = self.calls.lock().await;
        let (spec, loaded) = self.installed(&ctx).await?;
        let outcome = loaded
            .component
            .on_timer(
                self.environment(&ctx, &spec, &loaded, &self.own_principal()),
                &action.key,
            )
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

/// The last sequence number of `source` the installation has processed.
async fn event_cursor(ctx: &Ctx<Extension>, source: &str) -> Result<u64> {
    let result = ctx
        .sql()
        .query(
            "SELECT seq FROM _tartan_cursors WHERE source = ?",
            Some(vec![param_text(source)]),
        )
        .await?;
    match result.rows.first() {
        Some(row) => Ok(u64::try_from(crate::sql::integer(row, 0)?)?),
        None => Ok(0),
    }
}

/// A forge event as the extension world describes it.
fn to_extension_event(envelope: &Envelope, spec: &InstallationSpec) -> tartan_extensions::Event {
    tartan_extensions::Event {
        id: envelope.id.clone(),
        seq: envelope.seq,
        kind: envelope.kind.clone(),
        source: envelope.source.clone(),
        node: spec.repo_id.clone(),
        repo: Some(spec.repo_id.clone()),
        subject: None,
        actor: ExtensionActor {
            kind: ActorKind::System,
            id: "tartan".to_owned(),
            on_behalf_of: None,
        },
        caused_by: None,
        correlation: None,
        time_ms: u64::try_from(envelope.at_ms).unwrap_or_default(),
        depth: 0,
        shadow: false,
        data: envelope.data.to_string(),
    }
}

/// The context of a call: the installation's repository and the principal calling.
fn slot_context(spec: &InstallationSpec, slot: Option<&str>, viewer: &Caller) -> SlotContext {
    SlotContext {
        slot: slot.map(str::to_owned),
        node: spec.repo_id.clone(),
        repo: Some(spec.repo_id.clone()),
        git_ref: None,
        path: None,
        entity: None,
        viewer: Some(viewer.to_extension()),
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

/// Serves an installation's `calls` import: checks its manifest's `permissions.calls` and that
/// the target is on the caller's own repository, then calls the target installation's tool as
/// the same principal.
struct InstallationCaller {
    client: rivetkit::client::Client,
    /// The caller's repository, the only one it may call into.
    repo_id: String,
    /// `permissions.calls`: extension id to the tools it may call.
    allowed: Value,
    viewer: Caller,
}

#[async_trait]
impl ToolCaller for InstallationCaller {
    async fn call(
        &self,
        extension: &str,
        repo: Option<&str>,
        tool: &str,
        args: &str,
    ) -> Result<Result<String, ExtensionError>> {
        use rivetkit::TypedClientExt;
        use rivetkit::client::GetOptions;

        let permitted = self.allowed[extension]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t == tool));
        if !permitted {
            return Ok(Err(ExtensionError::Denied(format!(
                "permissions.calls does not allow {extension} {tool}"
            ))));
        }
        let Some(repo_id) = repo else {
            return Ok(Err(ExtensionError::Invalid(
                "repo is required: the caller's repository id".to_owned(),
            )));
        };
        if repo_id != self.repo_id {
            return Ok(Err(ExtensionError::Denied(
                "an installation may only call into its own repository".to_owned(),
            )));
        }
        let Ok(args) = serde_json::from_str::<Value>(args) else {
            return Ok(Err(ExtensionError::Invalid("args is not JSON".to_owned())));
        };
        let target = self.client.get_typed::<crate::Repo>(
            crate::actors::REPO,
            [repo_id],
            GetOptions::default(),
        )?;
        let installation = target
            .send(crate::InstallationQuery {
                extension_id: extension.to_owned(),
            })
            .await?;
        let Some(installation) = installation else {
            return Ok(Err(ExtensionError::NotFound(format!(
                "{extension} on repository {repo_id}"
            ))));
        };
        let reply = self
            .client
            .get_typed::<Extension>(
                crate::actors::EXTENSION,
                [installation.installation_id.as_str()],
                GetOptions::default(),
            )?
            .send(CallTool {
                tool: tool.to_owned(),
                args,
                viewer: self.viewer.clone(),
            })
            .await?;
        Ok(match reply {
            CallReply::Ok(value) => Ok(value.to_string()),
            CallReply::Failed { kind, message } => Err(match kind.as_str() {
                "denied" => ExtensionError::Denied(message),
                "not_found" => ExtensionError::NotFound(message),
                "invalid" => ExtensionError::Invalid(message),
                "conflict" => ExtensionError::Conflict(message),
                "unavailable" => ExtensionError::Unavailable(message),
                _ => ExtensionError::Internal(message),
            }),
        })
    }
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
