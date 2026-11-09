//! A repository's installations. They come only from configuration: the repository actor
//! reconciles its `installations` table, and the extension actors behind it, with the
//! extensions its committed package `tartan` names.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use rivetkit::TypedClientExt;
use rivetkit::client::GetOptions;
use rivetkit::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actors::extension::{Disable, Init as InitExtension, InstallationSpec};
use crate::event_log::append;
use crate::outbox::{Flush, Outbox, SubscriberSource};
use crate::sql::{integer, param_text, text};
use crate::{
    EventPattern, PackageInfo, PackageQuery, SubscribeInstallation, extension_handle, forge_handle,
};

/// The installations table. A removed installation keeps its row (disabled), so configuring it
/// again finds its id and its data.
pub(crate) const INSTALLATIONS_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS installations (
        extension_id TEXT PRIMARY KEY,
        installation_id TEXT NOT NULL UNIQUE,
        version TEXT NOT NULL,
        digest TEXT NOT NULL,
        config TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 0
    );
";

/// An extension installed on a repository.
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

/// An extension the configuration installs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Wanted {
    /// The version.
    pub(crate) version: String,
    /// The configuration the installation gets: its settings.
    pub(crate) config: Value,
}

/// The repository installations are made on.
pub(crate) struct InstallTarget {
    /// The org the repository belongs to.
    pub(crate) org_id: String,
    /// The repository.
    pub(crate) repo_id: String,
    /// `org/repo`.
    pub(crate) path: String,
}

/// Makes the installations match `wanted`, the configuration at commit `sha`.
///
/// The checks come first and are all or nothing: a configuration naming a package that is not
/// in `available` (the deployment's, as `id@version`), or a package whose `requires` it does not
/// also install, is refused, `config.refused` is recorded, and nothing changes. Then each
/// extension is initialised in turn, the extensions a package requires before it. If one
/// fails, the installations already changed stay changed, nothing is recorded, and the error is
/// returned: the repository actor applies the same configuration again later, and the
/// extensions already initialised are then unchanged, so the repository converges on the
/// configuration. Once every extension is initialised, `config.applied` is recorded in
/// `source`'s log. Returns whether the configuration was applied.
pub(crate) async fn reconcile<A>(
    ctx: &Ctx<A>,
    outbox: &Arc<Outbox>,
    source: &str,
    target: &InstallTarget,
    sha: &str,
    wanted: BTreeMap<String, Wanted>,
    available: &BTreeSet<String>,
) -> Result<bool>
where
    A: Actor + Handler<Flush>,
{
    let client = ctx.client()?;
    let forge = forge_handle(&client)?;
    let mut packages: BTreeMap<String, PackageInfo> = BTreeMap::new();
    let mut problems = Vec::new();
    for (extension_id, want) in &wanted {
        let package_name = format!("{extension_id}@{}", want.version);
        if !available.contains(&package_name) {
            problems.push(format!(
                "{package_name} is not available: no module the deployment lists holds it"
            ));
            continue;
        }
        let found = forge
            .send(PackageQuery {
                id: extension_id.clone(),
                version: want.version.clone(),
            })
            .await?;
        match found {
            None => problems.push(format!("{package_name} is not registered")),
            Some(package) => {
                problems.extend(missing_requirements(&package, &wanted));
                packages.insert(extension_id.clone(), package);
            }
        }
    }
    if !problems.is_empty() {
        refuse(ctx, outbox, source, sha, problems).await?;
        return Ok(false);
    }

    let pool = ctx.inner().client_pool_name().map(str::to_owned);
    let mut changes = Vec::new();
    let mut wanted = wanted;
    for extension_id in install_order(&packages) {
        let Some(want) = wanted.remove(&extension_id) else {
            continue;
        };
        let package = &packages[&extension_id];
        let current = row(ctx, &extension_id).await?;
        let config = want.config.to_string();
        let installation_id = current
            .as_ref()
            .map(|row| row.info.installation_id.clone())
            .unwrap_or_else(|| ulid::Ulid::new().to_string().to_lowercase());
        let change = match &current {
            None => "installed",
            Some(row) if !row.enabled => "installed",
            Some(row) if row.info.digest != package.digest => "upgraded",
            Some(row) if row.config != config => "configured",
            Some(_) => continue,
        };
        extension_handle(&client, &installation_id, pool.clone())?
            .send(InitExtension {
                spec: InstallationSpec {
                    installation_id: installation_id.clone(),
                    extension_id: extension_id.clone(),
                    version: package.version.clone(),
                    digest: package.digest.clone(),
                    config: config.clone(),
                    org_id: target.org_id.clone(),
                    repo_id: target.repo_id.clone(),
                    path: target.path.clone(),
                },
            })
            .await?;
        forge
            .send(SubscribeInstallation {
                installation_id: installation_id.clone(),
                patterns: subscriptions(&package.manifest)?,
                repo_id: target.repo_id.clone(),
                pool: pool.clone(),
            })
            .await?;
        ctx.sql()
            .execute(
                "INSERT INTO installations
                 (extension_id, installation_id, version, digest, config, enabled)
                 VALUES (?, ?, ?, ?, ?, 1)
                 ON CONFLICT (extension_id) DO UPDATE SET version = excluded.version,
                   digest = excluded.digest, config = excluded.config, enabled = 1",
                Some(vec![
                    param_text(extension_id.as_str()),
                    param_text(installation_id.as_str()),
                    param_text(package.version.as_str()),
                    param_text(package.digest.as_str()),
                    param_text(config.as_str()),
                ]),
            )
            .await?;
        changes.push((
            change,
            extension_id,
            installation_id,
            package.version.clone(),
        ));
    }

    for row in enabled_rows(ctx).await? {
        if packages.contains_key(&row.info.extension_id) {
            continue;
        }
        client
            .get_typed::<crate::Extension>(
                crate::actors::EXTENSION,
                [row.info.installation_id.as_str()],
                GetOptions::default(),
            )?
            .send(Disable {})
            .await?;
        forge
            .send(SubscribeInstallation {
                installation_id: row.info.installation_id.clone(),
                patterns: Vec::new(),
                repo_id: target.repo_id.clone(),
                pool: pool.clone(),
            })
            .await?;
        ctx.sql()
            .execute(
                "UPDATE installations SET enabled = 0 WHERE extension_id = ?",
                Some(vec![param_text(row.info.extension_id.as_str())]),
            )
            .await?;
        changes.push((
            "uninstalled",
            row.info.extension_id,
            row.info.installation_id,
            row.info.version,
        ));
    }

    let source = source.to_owned();
    let sha = sha.to_owned();
    ctx.sql()
        .transaction(
            move |tx| async move {
                let mut summary = Vec::new();
                for (change, extension_id, installation_id, version) in changes {
                    let data = json!({
                        "extension_id": extension_id,
                        "installation_id": installation_id,
                        "version": version,
                        "sha": sha,
                    });
                    append(&tx, &source, &format!("extension.{change}"), data).await?;
                    summary.push(json!({ "extension_id": extension_id, "change": change }));
                }
                let data = json!({ "sha": sha, "changes": summary });
                append(&tx, &source, "config.applied", data).await?;
                Ok(())
            },
            SqliteTransactionOptions {
                name: Some("config_applied"),
                timeout: None,
            },
        )
        .await?;
    outbox.flush_later(ctx, SubscriberSource::Forge);
    Ok(true)
}

/// Records that the configuration at `sha` was refused, and why; nothing else changes.
pub(crate) async fn refuse<A>(
    ctx: &Ctx<A>,
    outbox: &Arc<Outbox>,
    source: &str,
    sha: &str,
    problems: Vec<String>,
) -> Result<()>
where
    A: Actor + Handler<Flush>,
{
    let (source, data) = (
        source.to_owned(),
        json!({ "sha": sha, "problems": problems }),
    );
    ctx.sql()
        .transaction(
            move |tx| async move {
                append(&tx, &source, "config.refused", data).await?;
                Ok(())
            },
            SqliteTransactionOptions {
                name: Some("config_refused"),
                timeout: None,
            },
        )
        .await?;
    outbox.flush_later(ctx, SubscriberSource::Forge);
    Ok(())
}

/// A problem for each extension `package`'s manifest `requires` that `wanted` does not install.
fn missing_requirements(package: &PackageInfo, wanted: &BTreeMap<String, Wanted>) -> Vec<String> {
    package.manifest["requires"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|required| !wanted.contains_key(*required))
        .map(|required| {
            format!(
                "{} requires {required}, which the configuration does not install",
                package.id
            )
        })
        .collect()
}

/// The extensions of `packages` in the order they are initialised: each after the extensions
/// its manifest `requires`, otherwise by id.
fn install_order(packages: &BTreeMap<String, PackageInfo>) -> Vec<String> {
    fn visit(
        id: &str,
        packages: &BTreeMap<String, PackageInfo>,
        visiting: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if order.iter().any(|done| done == id) || !visiting.insert(id.to_owned()) {
            return;
        }
        if let Some(package) = packages.get(id) {
            for required in package.manifest["requires"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                visit(required, packages, visiting, order);
            }
            order.push(id.to_owned());
        }
    }
    let mut order = Vec::new();
    let mut visiting = BTreeSet::new();
    for id in packages.keys() {
        visit(id, packages, &mut visiting, &mut order);
    }
    order
}

/// The event patterns a manifest subscribes to.
fn subscriptions(manifest: &Value) -> Result<Vec<EventPattern>> {
    Ok(manifest["subscribe"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["event"].as_str())
                .map(str::parse::<EventPattern>)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default())
}

struct Row {
    info: InstallationInfo,
    config: String,
    enabled: bool,
}

fn row_from(row: &[rivetkit::ColumnValue]) -> Result<Row> {
    Ok(Row {
        info: InstallationInfo {
            extension_id: text(row, 0)?,
            installation_id: text(row, 1)?,
            version: text(row, 2)?,
            digest: text(row, 3)?,
        },
        config: text(row, 4)?,
        enabled: integer(row, 5)? != 0,
    })
}

const ROW_COLUMNS: &str = "extension_id, installation_id, version, digest, config, enabled";

async fn row<A: Actor>(ctx: &Ctx<A>, extension_id: &str) -> Result<Option<Row>> {
    let result = ctx
        .sql()
        .query(
            format!("SELECT {ROW_COLUMNS} FROM installations WHERE extension_id = ?"),
            Some(vec![param_text(extension_id)]),
        )
        .await?;
    result.rows.first().map(|r| row_from(r)).transpose()
}

async fn enabled_rows<A: Actor>(ctx: &Ctx<A>) -> Result<Vec<Row>> {
    let result = ctx
        .sql()
        .query(
            format!(
                "SELECT {ROW_COLUMNS} FROM installations WHERE enabled = 1 ORDER BY extension_id"
            ),
            None,
        )
        .await?;
    result.rows.iter().map(|r| row_from(r)).collect()
}

/// The enabled installation of `extension_id`.
pub(crate) async fn ready<A: Actor>(
    ctx: &Ctx<A>,
    extension_id: &str,
) -> Result<Option<InstallationInfo>> {
    Ok(row(ctx, extension_id)
        .await?
        .filter(|row| row.enabled)
        .map(|row| row.info))
}

/// Every enabled installation.
pub(crate) async fn list<A: Actor>(ctx: &Ctx<A>) -> Result<Vec<InstallationInfo>> {
    Ok(enabled_rows(ctx)
        .await?
        .into_iter()
        .map(|row| row.info)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(id: &str, requires: Value) -> PackageInfo {
        PackageInfo {
            id: id.to_owned(),
            version: "0.1.0".to_owned(),
            digest: "d".to_owned(),
            manifest: json!({ "id": id, "requires": requires }),
        }
    }

    fn wanted(ids: &[&str]) -> BTreeMap<String, Wanted> {
        ids.iter()
            .map(|id| {
                let want = Wanted {
                    version: "0.1.0".to_owned(),
                    config: json!({}),
                };
                ((*id).to_owned(), want)
            })
            .collect()
    }

    #[test]
    fn required_extensions_are_initialised_first() {
        let packages: BTreeMap<String, PackageInfo> = [
            package("a.board", json!(["z.issues"])),
            package("m.reports", json!(["a.board", "z.issues"])),
            package("z.issues", Value::Null),
            package("b.loop", json!(["c.loop"])),
            package("c.loop", json!(["b.loop"])),
        ]
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
        assert_eq!(
            install_order(&packages),
            vec!["z.issues", "a.board", "c.loop", "b.loop", "m.reports"]
        );
    }

    #[test]
    fn requirements_must_be_installed_by_the_same_configuration() {
        let kanban = package(
            "kanban.tartan.rawkode.academy",
            json!(["issues.tartan.rawkode.academy"]),
        );
        assert!(
            missing_requirements(
                &kanban,
                &wanted(&[
                    "kanban.tartan.rawkode.academy",
                    "issues.tartan.rawkode.academy"
                ])
            )
            .is_empty()
        );
        assert_eq!(
            missing_requirements(&kanban, &wanted(&["kanban.tartan.rawkode.academy"])),
            vec![
                "kanban.tartan.rawkode.academy requires issues.tartan.rawkode.academy, which the configuration does not install"
            ]
        );
        let alone = package("issues.tartan.rawkode.academy", Value::Null);
        assert!(
            missing_requirements(&alone, &wanted(&["issues.tartan.rawkode.academy"])).is_empty()
        );
    }
}
