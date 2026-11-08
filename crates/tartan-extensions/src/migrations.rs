use anyhow::{Context, Result};

use crate::{SqlBackend, SqlValue};

/// Applies, in order, each migration not applied before, recording each by name in the host's
/// `_tartan_migrations` table. Migration names must stay stable across package versions.
pub async fn apply_migrations(sql: &dyn SqlBackend, migrations: &[(String, String)]) -> Result<()> {
    sql.exec(
        "CREATE TABLE IF NOT EXISTS _tartan_migrations (name TEXT PRIMARY KEY, applied_at_ms INTEGER NOT NULL)",
        Vec::new(),
    )
    .await?;
    for (name, script) in migrations {
        let applied = sql
            .exec(
                "SELECT 1 FROM _tartan_migrations WHERE name = ?",
                vec![SqlValue::Text(name.clone())],
            )
            .await?;
        if !applied.values.is_empty() {
            continue;
        }
        sql.exec_script(script)
            .await
            .with_context(|| format!("migration {name}"))?;
        sql.exec(
            "INSERT INTO _tartan_migrations (name, applied_at_ms) VALUES (?, ?)",
            vec![
                SqlValue::Text(name.clone()),
                SqlValue::Integer(crate::host_state::now_ms()),
            ],
        )
        .await?;
    }
    Ok(())
}
