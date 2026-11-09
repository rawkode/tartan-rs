//! Typed access to rows returned by the actor's SQLite database.

use anyhow::{Result, anyhow};
use rivetkit::{BindParam, ColumnValue};

/// Column `index` of `row` as text.
pub(crate) fn text(row: &[ColumnValue], index: usize) -> Result<String> {
    match row.get(index) {
        Some(ColumnValue::Text(value)) => Ok(value.clone()),
        other => Err(anyhow!("column {index}: expected text, got {other:?}")),
    }
}

/// Column `index` of `row` as text, or `None` for NULL.
pub(crate) fn optional_text(row: &[ColumnValue], index: usize) -> Result<Option<String>> {
    match row.get(index) {
        Some(ColumnValue::Null) => Ok(None),
        _ => text(row, index).map(Some),
    }
}

/// Column `index` of `row` as an integer.
pub(crate) fn integer(row: &[ColumnValue], index: usize) -> Result<i64> {
    match row.get(index) {
        Some(ColumnValue::Integer(value)) => Ok(*value),
        other => Err(anyhow!(
            "column {index}: expected an integer, got {other:?}"
        )),
    }
}

/// A text parameter.
pub(crate) fn param_text(value: impl Into<String>) -> BindParam {
    BindParam::Text(value.into())
}

/// An optional text parameter: NULL for `None`.
pub(crate) fn param_optional_text(value: Option<String>) -> BindParam {
    value.map_or(BindParam::Null, BindParam::Text)
}

/// An integer parameter.
pub(crate) fn param_integer(value: i64) -> BindParam {
    BindParam::Integer(value)
}

/// A blob parameter.
pub(crate) fn param_blob(value: impl Into<Vec<u8>>) -> BindParam {
    BindParam::Blob(value.into())
}

/// Column `index` of `row` as an integer, or `None` for NULL.
pub(crate) fn optional_integer(row: &[ColumnValue], index: usize) -> Result<Option<i64>> {
    match row.get(index) {
        Some(ColumnValue::Null) => Ok(None),
        _ => integer(row, index).map(Some),
    }
}

/// Milliseconds since the Unix epoch, from the installed clock ([`crate::clock`]).
pub(crate) fn now_ms() -> i64 {
    crate::clock::now_ms()
}
