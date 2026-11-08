//! The Issues extension (`rawkode.issues`) runs on this host unchanged.
//!
//! Needs `TARTAN_ISSUES_PACKAGE`, a package directory from `scripts/build-extension.sh`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use rusqlite::types::{Value as RowValue, ValueRef};
use serde_json::{Value, json};
use tartan_extensions::{
    Actor, ActorKind, CallEnvironment, CallLimits, ExtensionPackage, ExtensionRuntime, InstallMode,
    SlotContext, SqlBackend, SqlRows, SqlValue, apply_migrations,
};

/// An in-memory SQLite database.
struct MemorySql(Mutex<rusqlite::Connection>);

impl MemorySql {
    fn new() -> Arc<Self> {
        let connection = rusqlite::Connection::open_in_memory().expect("open SQLite");
        Arc::new(Self(Mutex::new(connection)))
    }
}

#[async_trait]
impl SqlBackend for MemorySql {
    async fn exec(&self, query: &str, params: Vec<SqlValue>) -> Result<SqlRows> {
        let connection = self.0.lock().expect("never poisoned");
        let mut statement = connection.prepare(query)?;
        let params: Vec<RowValue> = params
            .into_iter()
            .map(|value| match value {
                SqlValue::Null => RowValue::Null,
                SqlValue::Integer(v) => RowValue::Integer(v),
                SqlValue::Real(v) => RowValue::Real(v),
                SqlValue::Text(v) => RowValue::Text(v),
                SqlValue::Blob(v) => RowValue::Blob(v),
            })
            .collect();
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let width = columns.len();
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        let mut values = Vec::new();
        while let Some(row) = rows.next()? {
            let mut out = Vec::with_capacity(width);
            for index in 0..width {
                out.push(match row.get_ref(index)? {
                    ValueRef::Null => SqlValue::Null,
                    ValueRef::Integer(v) => SqlValue::Integer(v),
                    ValueRef::Real(v) => SqlValue::Real(v),
                    ValueRef::Text(v) => SqlValue::Text(String::from_utf8_lossy(v).into_owned()),
                    ValueRef::Blob(v) => SqlValue::Blob(v.to_vec()),
                });
            }
            values.push(out);
        }
        drop(rows);
        // SQLite reports the last write's count; a query writes nothing.
        let rows_written = if width == 0 { connection.changes() } else { 0 };
        Ok(SqlRows {
            columns,
            values,
            rows_written,
        })
    }

    async fn exec_script(&self, script: &str) -> Result<()> {
        self.0
            .lock()
            .expect("never poisoned")
            .execute_batch(script)?;
        Ok(())
    }
}

fn context() -> SlotContext {
    SlotContext {
        slot: None,
        node: "node-1".to_owned(),
        repo: Some("repo-1".to_owned()),
        git_ref: None,
        path: None,
        entity: None,
        viewer: Some(Actor {
            kind: ActorKind::User,
            id: "alice".to_owned(),
            on_behalf_of: None,
        }),
        mode: InstallMode::Enforce,
        extra: None,
    }
}

fn env(sql: &Arc<MemorySql>, limits: CallLimits) -> CallEnvironment {
    CallEnvironment {
        sql: sql.clone(),
        config: "{}".to_owned(),
        limits,
        label: "rawkode.issues@test".to_owned(),
        caller: None,
    }
}

fn package() -> ExtensionPackage {
    let dir = std::env::var("TARTAN_ISSUES_PACKAGE")
        .expect("TARTAN_ISSUES_PACKAGE names a package directory");
    ExtensionPackage::from_dir(&PathBuf::from(dir)).expect("a valid package")
}

#[tokio::test]
#[ignore = "needs TARTAN_ISSUES_PACKAGE; run scripts/e2e.sh"]
async fn issues_creates_lists_and_renders() {
    let package = package();
    assert_eq!(package.manifest.id, "rawkode.issues");
    let runtime = ExtensionRuntime::new().expect("runtime");
    let component = runtime.compile(&package.component).expect("compile");
    let sql = MemorySql::new();
    apply_migrations(sql.as_ref(), &package.migrations)
        .await
        .expect("migrations");
    // Applying them again changes nothing.
    apply_migrations(sql.as_ref(), &package.migrations)
        .await
        .expect("migrations again");

    let limits = CallLimits::default();
    let init = component.init(env(&sql, limits)).await.expect("call init");
    assert!(init.result.is_ok(), "{:?}", init.result);

    let created = component
        .call_tool(
            env(&sql, limits),
            "create",
            &json!({"title": "The first issue", "body": "Hello"}).to_string(),
            &context(),
        )
        .await
        .expect("call create");
    let created: Value = serde_json::from_str(&created.result.expect("created")).expect("json");
    assert_eq!(created["issue"]["number"], 1);
    assert_eq!(created["issue"]["title"], "The first issue");

    let listed = component
        .call_tool(env(&sql, limits), "list", "{}", &context())
        .await
        .expect("call list");
    let listed: Value = serde_json::from_str(&listed.result.expect("listed")).expect("json");
    assert_eq!(listed["open"], 1);
    assert_eq!(listed["issues"][0]["title"], "The first issue");

    let page = component
        .render(env(&sql, limits), "issues", &context(), "{}")
        .await
        .expect("call render");
    let page = page.result.expect("rendered");
    assert!(page.contains("The first issue"), "{page}");

    let unknown = component
        .call_tool(env(&sql, limits), "nope", "{}", &context())
        .await
        .expect("call");
    assert!(unknown.result.is_err());
}

#[tokio::test]
#[ignore = "needs TARTAN_ISSUES_PACKAGE; run scripts/e2e.sh"]
async fn a_call_out_of_fuel_fails_without_effects() {
    let package = package();
    let runtime = ExtensionRuntime::new().expect("runtime");
    let component = runtime.compile(&package.component).expect("compile");
    let sql = MemorySql::new();
    apply_migrations(sql.as_ref(), &package.migrations)
        .await
        .expect("migrations");
    let starved = CallLimits {
        fuel: 1_000,
        ..CallLimits::default()
    };
    let result = component
        .call_tool(env(&sql, starved), "list", "{}", &context())
        .await;
    assert!(result.is_err(), "a call without fuel traps");
}

#[test]
fn a_component_outside_the_world_is_refused() {
    let runtime = ExtensionRuntime::new().expect("runtime");
    assert!(runtime.compile(b"not wasm").is_err());
}
