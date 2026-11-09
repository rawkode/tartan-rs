use anyhow::Result;
use async_trait::async_trait;

/// One SQLite value.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    /// NULL.
    Null,
    /// A 64-bit integer.
    Integer(i64),
    /// A 64-bit float.
    Real(f64),
    /// Text.
    Text(String),
    /// A blob.
    Blob(Vec<u8>),
}

/// The result of one statement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SqlRows {
    /// The result's column names.
    pub columns: Vec<String>,
    /// The rows, each with one value per column.
    pub values: Vec<Vec<SqlValue>>,
    /// How many rows the statement inserted, updated or deleted.
    pub rows_written: u64,
}

/// An installation's SQLite database.
#[async_trait]
pub trait SqlBackend: Send + Sync {
    /// Runs one statement with positional parameters.
    async fn exec(&self, query: &str, params: Vec<SqlValue>) -> Result<SqlRows>;

    /// Runs one statement that SQLite itself keeps from changing the database, failing when the
    /// statement would write. Read-only calls use this, so a statement that only looks like a
    /// query, such as `WITH a AS (SELECT 1) DELETE FROM t`, is refused by the engine.
    async fn exec_read_only(&self, query: &str, params: Vec<SqlValue>) -> Result<SqlRows>;

    /// Runs a script of statements without parameters, such as a migration.
    async fn exec_script(&self, script: &str) -> Result<()>;
}

/// Calls a tool of another extension's installation for the `calls` import.
#[async_trait]
pub trait ToolCaller: Send + Sync {
    /// Calls `tool` of `extension` (in repository `repo`, for a repository's installation)
    /// with JSON `args`. `Ok(Err(_))` is the callee's or the permission check's refusal; `Err`
    /// is a failure to reach it.
    async fn call(
        &self,
        extension: &str,
        repo: Option<&str>,
        tool: &str,
        args: &str,
    ) -> Result<Result<String, crate::ExtensionError>>;
}

/// The prefix of every table the host keeps in an installation's database.
pub(crate) const HOST_TABLE_PREFIX: &str = "_tartan_";

/// Why the extension's statement `query` may not run, or `None` when it may.
///
/// This is a lexical guard against mistakes, not a sandbox: it keeps an extension's statements off
/// the host's tables and away from statements that change the connection, and in a read-only call
/// it admits only a single statement starting with `SELECT` or `WITH`. A read-only call's
/// statement then runs through [`SqlBackend::exec_read_only`], where SQLite refuses any write
/// (a `WITH` prefix can introduce an `INSERT`, `UPDATE` or `DELETE`).
pub fn check_statement(query: &str, read_only: bool) -> Option<&'static str> {
    let lowered = query.to_ascii_lowercase();
    let trimmed = lowered.trim_start();
    if lowered.contains(HOST_TABLE_PREFIX) {
        return Some("the host's tables are not accessible");
    }
    let leading = trimmed.split_whitespace().next().unwrap_or_default();
    if matches!(leading, "attach" | "detach" | "pragma" | "vacuum") {
        return Some("this statement is not allowed");
    }
    if read_only {
        let single = !trimmed.trim_end().trim_end_matches(';').contains(';');
        if !single || !matches!(leading, "select" | "with") {
            return Some("read-only");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::check_statement;

    #[test]
    fn guards_host_tables_and_connection_statements() {
        assert!(check_statement("SELECT * FROM issues", false).is_none());
        assert!(check_statement("select * from _TARTAN_kv", false).is_some());
        assert!(check_statement("  PRAGMA query_only = 0", false).is_some());
        assert!(check_statement("ATTACH 'x' AS y", false).is_some());
    }

    #[test]
    fn read_only_calls_admit_one_query() {
        assert!(check_statement("SELECT 1", true).is_none());
        assert!(check_statement("WITH a AS (SELECT 1) SELECT * FROM a;", true).is_none());
        assert!(check_statement("INSERT INTO t VALUES (1)", true).is_some());
        assert!(check_statement("SELECT 1; DELETE FROM t", true).is_some());
    }
}
