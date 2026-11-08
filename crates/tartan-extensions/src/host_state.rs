//! The host side of the `tartan:ext` world for one call.

use std::sync::Arc;

use wasmtime::StoreLimits;

use crate::bindings::{effects, host, kv, sql, types};
use crate::{SqlBackend, SqlRows, SqlValue, check_statement};

/// Whether a call may change the installation's data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    /// `render` and `provide-context`: SQL may only read, and effects are refused.
    ReadOnly,
    /// Every other export.
    ReadWrite,
}

/// Something a call asked for that the caller applies once the call has succeeded.
#[derive(Clone, Debug)]
pub enum Effect {
    /// Emit the event `kind` about `subject` with the JSON payload `data`.
    Emit {
        /// The event type, relative to the extension (`issue.created`).
        kind: String,
        /// What the event is about.
        subject: Option<types::EntityRef>,
        /// The JSON payload.
        data: String,
    },
    /// Call `on-timer` with `key` at `at_ms` (milliseconds since the Unix epoch).
    SetTimer {
        /// The timer's key; setting it again replaces it.
        key: String,
        /// When.
        at_ms: u64,
    },
}

/// The largest buffer `host.random` returns.
const RANDOM_MAX: u32 = 4096;
/// The most keys `kv.list-keys` returns.
const KV_LIST_MAX: u32 = 1000;

/// What a call's host functions reach.
pub(crate) struct HostState {
    pub(crate) sql: Arc<dyn SqlBackend>,
    pub(crate) config: String,
    pub(crate) kind: CallKind,
    pub(crate) effects: Vec<Effect>,
    pub(crate) limits: StoreLimits,
    /// Names the installation in logs.
    pub(crate) label: String,
    kv_ready: bool,
}

impl HostState {
    pub(crate) fn new(
        sql: Arc<dyn SqlBackend>,
        config: String,
        kind: CallKind,
        limits: StoreLimits,
        label: String,
    ) -> Self {
        Self {
            sql,
            config,
            kind,
            effects: Vec::new(),
            limits,
            label,
            kv_ready: false,
        }
    }

    fn read_only(&self) -> bool {
        self.kind == CallKind::ReadOnly
    }

    /// Runs a host statement, which the guard does not apply to.
    async fn host_exec(&mut self, query: &str, params: Vec<SqlValue>) -> wasmtime::Result<SqlRows> {
        if !self.kv_ready {
            self.sql
                .exec(
                    "CREATE TABLE IF NOT EXISTS _tartan_kv (key TEXT PRIMARY KEY, value BLOB NOT NULL)",
                    Vec::new(),
                )
                .await
                .map_err(wasmtime::Error::from_anyhow)?;
            self.kv_ready = true;
        }
        self.sql
            .exec(query, params)
            .await
            .map_err(wasmtime::Error::from_anyhow)
    }
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

impl types::Host for HostState {}

impl sql::Host for HostState {
    async fn exec(
        &mut self,
        query: String,
        params: Vec<sql::Value>,
    ) -> wasmtime::Result<Result<sql::Rows, types::Error>> {
        if let Some(reason) = check_statement(&query, self.read_only()) {
            return Ok(Err(types::Error::Denied(reason.to_owned())));
        }
        let params = params.into_iter().map(from_wit).collect();
        match self.sql.exec(&query, params).await {
            Ok(rows) => Ok(Ok(sql::Rows {
                columns: rows.columns,
                values: rows
                    .values
                    .into_iter()
                    .map(|row| row.into_iter().map(to_wit).collect())
                    .collect(),
                rows_written: rows.rows_written,
            })),
            Err(error) => Ok(Err(types::Error::Invalid(format!("{error:#}")))),
        }
    }
}

fn from_wit(value: sql::Value) -> SqlValue {
    match value {
        sql::Value::Null => SqlValue::Null,
        sql::Value::Integer(v) => SqlValue::Integer(v),
        sql::Value::Real(v) => SqlValue::Real(v),
        sql::Value::Text(v) => SqlValue::Text(v),
        sql::Value::Blob(v) => SqlValue::Blob(v),
    }
}

fn to_wit(value: SqlValue) -> sql::Value {
    match value {
        SqlValue::Null => sql::Value::Null,
        SqlValue::Integer(v) => sql::Value::Integer(v),
        SqlValue::Real(v) => sql::Value::Real(v),
        SqlValue::Text(v) => sql::Value::Text(v),
        SqlValue::Blob(v) => sql::Value::Blob(v),
    }
}

impl kv::Host for HostState {
    async fn get(&mut self, key: String) -> wasmtime::Result<Option<Vec<u8>>> {
        let rows = self
            .host_exec(
                "SELECT value FROM _tartan_kv WHERE key = ?",
                vec![SqlValue::Text(key)],
            )
            .await?;
        Ok(
            match rows
                .values
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().next())
            {
                Some(SqlValue::Blob(value)) => Some(value),
                _ => None,
            },
        )
    }

    async fn put(&mut self, key: String, value: Vec<u8>) -> wasmtime::Result<()> {
        if self.read_only() {
            wasmtime::bail!("kv.put in a read-only call");
        }
        self.host_exec(
            "INSERT INTO _tartan_kv (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            vec![SqlValue::Text(key), SqlValue::Blob(value)],
        )
        .await?;
        Ok(())
    }

    async fn delete(&mut self, key: String) -> wasmtime::Result<bool> {
        if self.read_only() {
            wasmtime::bail!("kv.delete in a read-only call");
        }
        let rows = self
            .host_exec(
                "DELETE FROM _tartan_kv WHERE key = ?",
                vec![SqlValue::Text(key)],
            )
            .await?;
        Ok(rows.rows_written > 0)
    }

    async fn list_keys(&mut self, prefix: String, limit: u32) -> wasmtime::Result<Vec<String>> {
        // `prefix` is matched with substr, not LIKE, so `%` and `_` in it are literal.
        let rows = self
            .host_exec(
                "SELECT key FROM _tartan_kv WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key LIMIT ?2",
                vec![
                    SqlValue::Text(prefix),
                    SqlValue::Integer(i64::from(limit.min(KV_LIST_MAX))),
                ],
            )
            .await?;
        Ok(rows
            .values
            .into_iter()
            .filter_map(|row| match row.into_iter().next() {
                Some(SqlValue::Text(key)) => Some(key),
                _ => None,
            })
            .collect())
    }
}

impl host::Host for HostState {
    async fn log(&mut self, level: host::Level, msg: String) -> wasmtime::Result<()> {
        let installation = self.label.as_str();
        match level {
            host::Level::Debug => tracing::debug!(installation, "{msg}"),
            host::Level::Info => tracing::info!(installation, "{msg}"),
            host::Level::Warn => tracing::warn!(installation, "{msg}"),
            host::Level::Error => tracing::error!(installation, "{msg}"),
        }
        Ok(())
    }

    async fn config(&mut self) -> wasmtime::Result<String> {
        Ok(self.config.clone())
    }

    async fn now_ms(&mut self) -> wasmtime::Result<u64> {
        Ok(u64::try_from(now_ms()).unwrap_or_default())
    }

    async fn random(&mut self, len: u32) -> wasmtime::Result<Vec<u8>> {
        let mut bytes = vec![0; usize::try_from(len.min(RANDOM_MAX))?];
        getrandom::fill(&mut bytes).map_err(|error| wasmtime::Error::msg(error.to_string()))?;
        Ok(bytes)
    }

    async fn ulid(&mut self) -> wasmtime::Result<String> {
        Ok(ulid::Ulid::new().to_string().to_lowercase())
    }
}

impl effects::Host for HostState {
    async fn emit(
        &mut self,
        kind: String,
        subject: Option<types::EntityRef>,
        data: String,
    ) -> wasmtime::Result<Result<(), types::Error>> {
        if self.read_only() {
            return Ok(Err(types::Error::Denied("read-only".to_owned())));
        }
        if serde_json::from_str::<serde_json::Value>(&data).is_err() {
            return Ok(Err(types::Error::Invalid("data is not JSON".to_owned())));
        }
        self.effects.push(Effect::Emit {
            kind,
            subject,
            data,
        });
        Ok(Ok(()))
    }

    async fn notify(
        &mut self,
        _principal: String,
        _options: types::NotifyOptions,
        _text: String,
        _data: String,
    ) -> wasmtime::Result<Result<(), types::Error>> {
        Ok(Err(types::Error::Unavailable(
            "notifications are not supported by this host yet".to_owned(),
        )))
    }

    async fn contribute_note(
        &mut self,
        _change_id: String,
        _section: String,
    ) -> wasmtime::Result<Result<(), types::Error>> {
        Ok(Err(types::Error::Unavailable(
            "why-notes are not supported by this host yet".to_owned(),
        )))
    }

    async fn set_timer(&mut self, key: String, at_ms: u64) -> wasmtime::Result<()> {
        if self.read_only() {
            wasmtime::bail!("effects.set-timer in a read-only call");
        }
        self.effects.push(Effect::SetTimer { key, at_ms });
        Ok(())
    }
}
