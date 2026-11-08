//! Installing extensions, shared by the org and repo actors: each keeps the installations made
//! on it in its own `installations` table.

use anyhow::{Result, bail};
use rivetkit::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actors::extension::{Init as InitExtension, InstallationSpec};
use crate::event_log::append;
use crate::outbox::{Outbox, SubscriberSource};
use crate::sql::{integer, param_text, text};
use crate::{
    EventPattern, PackageQuery, RecipientScope, SubscribeInstallation, extension_handle,
    forge_handle,
};

/// The installations table.
pub(crate) const INSTALLATIONS_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS installations (
        extension_id TEXT PRIMARY KEY,
        installation_id TEXT NOT NULL UNIQUE,
        version TEXT NOT NULL,
        digest TEXT NOT NULL,
        ready INTEGER NOT NULL DEFAULT 0
    );
";

/// An extension installed on an org or a repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationInfo {
    /// The installation's id: its extension actor's key.
    pub installation_id: String,
    /// The extension's id.
    pub extension_id: String,
    /// The installed version.
    pub version: String,
    /// The package digest.
    pub digest: String,
}

/// Installs a published extension package, or returns the existing installation of that
/// extension. One version of an extension is installed at a time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Install {
    /// The extension's id.
    pub extension_id: String,
    /// The version to install.
    pub version: String,
    /// The installation's configuration.
    pub config: serde_json::Value,
}

impl Action for Install {
    type Output = InstallationInfo;

    const NAME: &'static str = "install";
}

/// Finds the installation of an extension.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallationQuery {
    /// The extension's id.
    pub extension_id: String,
}

impl Action for InstallationQuery {
    type Output = Option<InstallationInfo>;

    const NAME: &'static str = "installation";
}

/// Every installation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListInstallations {}

impl Action for ListInstallations {
    type Output = Vec<InstallationInfo>;

    const NAME: &'static str = "listInstallations";
}

/// Where an installation is made.
pub(crate) struct InstallTarget {
    /// `repo` or `org`: the storage scope the package must declare.
    pub(crate) scope: &'static str,
    /// The org the installation belongs to.
    pub(crate) org_id: String,
    /// The repository, for a repository's installation.
    pub(crate) repo_id: Option<String>,
    /// `org` or `org/repo`.
    pub(crate) path: String,
    /// The events the installation may receive.
    pub(crate) receives: RecipientScope,
}

/// Installs `action`'s package on `target`; `source` names the installing actor's event log.
pub(crate) async fn install<A>(
    ctx: &Ctx<A>,
    outbox: &std::sync::Arc<Outbox>,
    source: &str,
    target: InstallTarget,
    action: Install,
) -> Result<InstallationInfo>
where
    A: Actor + Handler<crate::outbox::Flush>,
{
    let client = ctx.client()?;
    let Some(package) = forge_handle(&client)?
        .send(PackageQuery {
            id: action.extension_id.clone(),
            version: action.version.clone(),
        })
        .await?
    else {
        bail!(
            "{}@{} is not published",
            action.extension_id,
            action.version
        );
    };
    if package.manifest["storage"]["scope"] != target.scope {
        bail!(
            "{} is not installed per {}",
            action.extension_id,
            target.scope
        );
    }
    if find(ctx, &action.extension_id).await?.is_none() {
        let installation_id = ulid::Ulid::new().to_string().to_lowercase();
        ctx.sql()
            .execute(
                "INSERT OR IGNORE INTO installations
                 (extension_id, installation_id, version, digest) VALUES (?, ?, ?, ?)",
                Some(vec![
                    param_text(action.extension_id.as_str()),
                    param_text(installation_id),
                    param_text(package.version.as_str()),
                    param_text(package.digest.as_str()),
                ]),
            )
            .await?;
    }
    let Some((info, ready)) = find(ctx, &action.extension_id).await? else {
        bail!("installation of {} vanished", action.extension_id);
    };
    if ready {
        return Ok(info);
    }

    // The extension actor is initialized and subscribed first, then the installation is marked
    // ready, so a retry reuses the pending id; both steps repeat harmlessly.
    let pool = ctx.inner().client_pool_name().map(str::to_owned);
    extension_handle(&client, &info.installation_id, pool.clone())?
        .send(InitExtension {
            spec: InstallationSpec {
                installation_id: info.installation_id.clone(),
                extension_id: info.extension_id.clone(),
                version: info.version.clone(),
                digest: info.digest.clone(),
                config: action.config.to_string(),
                org_id: target.org_id.clone(),
                repo_id: target.repo_id.clone(),
                path: target.path.clone(),
            },
        })
        .await?;
    let patterns = package.manifest["subscribe"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["event"].as_str())
                .map(str::parse::<EventPattern>)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    if !patterns.is_empty() {
        forge_handle(&client)?
            .send(SubscribeInstallation {
                installation_id: info.installation_id.clone(),
                patterns,
                receives: target.receives,
                pool,
            })
            .await?;
    }

    let source = source.to_owned();
    let event_info = info.clone();
    ctx.sql()
        .transaction(
            move |tx| async move {
                let marked = tx
                    .execute(
                        "UPDATE installations SET ready = 1 WHERE extension_id = ? AND ready = 0",
                        Some(vec![param_text(event_info.extension_id.as_str())]),
                    )
                    .await?;
                if marked.changes == 1 {
                    let data = json!({
                        "installation_id": event_info.installation_id,
                        "extension_id": event_info.extension_id,
                        "version": event_info.version,
                    });
                    append(&tx, &source, "extension.installed", data).await?;
                }
                Ok(())
            },
            SqliteTransactionOptions {
                name: Some("install"),
                timeout: None,
            },
        )
        .await?;
    outbox.flush_later(ctx, SubscriberSource::Forge);
    Ok(info)
}

/// The installation of `extension_id`, and whether it is ready.
pub(crate) async fn find<A: Actor>(
    ctx: &Ctx<A>,
    extension_id: &str,
) -> Result<Option<(InstallationInfo, bool)>> {
    let result = ctx
        .sql()
        .query(
            "SELECT installation_id, version, digest, ready FROM installations
             WHERE extension_id = ?",
            Some(vec![param_text(extension_id)]),
        )
        .await?;
    let Some(row) = result.rows.first() else {
        return Ok(None);
    };
    let info = InstallationInfo {
        installation_id: text(row, 0)?,
        extension_id: extension_id.to_owned(),
        version: text(row, 1)?,
        digest: text(row, 2)?,
    };
    Ok(Some((info, integer(row, 3)? != 0)))
}

/// The ready installation of `extension_id`.
pub(crate) async fn ready<A: Actor>(
    ctx: &Ctx<A>,
    extension_id: &str,
) -> Result<Option<InstallationInfo>> {
    Ok(find(ctx, extension_id)
        .await?
        .and_then(|(info, ready)| ready.then_some(info)))
}

/// Every ready installation.
pub(crate) async fn list<A: Actor>(ctx: &Ctx<A>) -> Result<Vec<InstallationInfo>> {
    let result = ctx
        .sql()
        .query(
            "SELECT installation_id, extension_id, version, digest FROM installations
             WHERE ready = 1 ORDER BY extension_id",
            None,
        )
        .await?;
    result
        .rows
        .iter()
        .map(|row| {
            Ok(InstallationInfo {
                installation_id: text(row, 0)?,
                extension_id: text(row, 1)?,
                version: text(row, 2)?,
                digest: text(row, 3)?,
            })
        })
        .collect()
}
