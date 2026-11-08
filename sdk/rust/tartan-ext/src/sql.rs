//! The extension's own SQLite (the facet's database), synchronous.
//!
//! Statements run as written with positional `?` bindings. The host rejects
//! statements that name `_`- or `sqlite_`-prefixed objects and anything but
//! DML and DDL; in a read-only call (`render`, `provide-context`) only one
//! `SELECT`/`WITH … SELECT`/`VALUES` statement runs, and a statement that
//! wrote anything after all is rolled back with `denied("read-only")`.
//! Above the installation's storage quota, statements that grow the
//! database get `denied("quota")`.
//!
//! ```ignore
//! use tartan_ext::sql::{self, params};
//!
//! sql::execute("INSERT INTO findings (path, line) VALUES (?, ?)", &params!["a.env", 3])?;
//! for row in sql::query("SELECT path, line FROM findings", &[])? {
//!     let path = row.text("path").unwrap_or_default();
//!     let line = row.int("line").unwrap_or(0);
//! }
//! ```

use std::rc::Rc;

use crate::bindings::tartan::ext::sql as raw;
use crate::Error;

pub use raw::{Rows, Value};

/// Runs one statement and returns its raw rows.
pub fn exec(query: &str, params: &[Value]) -> Result<Rows, Error> {
	raw::exec(query, params)
}

/// Runs a write and returns the number of rows it wrote.
pub fn execute(query: &str, params: &[Value]) -> Result<u64, Error> {
	raw::exec(query, params).map(|rows| rows.rows_written)
}

/// Runs a query and returns its rows with by-name access.
pub fn query(query: &str, params: &[Value]) -> Result<Vec<Row>, Error> {
	raw::exec(query, params).map(Row::all)
}

/// The first row of a query, if any.
pub fn first(query: &str, params: &[Value]) -> Result<Option<Row>, Error> {
	Ok(self::query(query, params)?.into_iter().next())
}

/// One result row; values are looked up by column name.
#[derive(Clone, Debug)]
pub struct Row {
	columns: Rc<Vec<String>>,
	values: Vec<Value>,
}

impl Row {
	/// Every row of a result, sharing the column names.
	pub fn all(rows: Rows) -> Vec<Row> {
		let columns = Rc::new(rows.columns);
		rows.values
			.into_iter()
			.map(|values| Row {
				columns: Rc::clone(&columns),
				values,
			})
			.collect()
	}

	pub fn columns(&self) -> &[String] {
		&self.columns
	}

	pub fn get(&self, column: &str) -> Option<&Value> {
		let index = self.columns.iter().position(|c| c == column)?;
		self.values.get(index)
	}

	/// The text value of a column (None for NULL, numbers or blobs).
	pub fn text(&self, column: &str) -> Option<&str> {
		match self.get(column)? {
			Value::Text(s) => Some(s.as_str()),
			_ => None,
		}
	}

	/// The integer value of a column (a real is truncated toward zero).
	pub fn int(&self, column: &str) -> Option<i64> {
		match self.get(column)? {
			Value::Integer(i) => Some(*i),
			Value::Real(r) => Some(*r as i64),
			_ => None,
		}
	}

	pub fn real(&self, column: &str) -> Option<f64> {
		match self.get(column)? {
			Value::Real(r) => Some(*r),
			Value::Integer(i) => Some(*i as f64),
			_ => None,
		}
	}

	pub fn blob(&self, column: &str) -> Option<&[u8]> {
		match self.get(column)? {
			Value::Blob(b) => Some(b.as_slice()),
			_ => None,
		}
	}

	pub fn is_null(&self, column: &str) -> bool {
		matches!(self.get(column), Some(Value::Null) | None)
	}
}

impl From<&str> for Value {
	fn from(v: &str) -> Self {
		Value::Text(v.to_string())
	}
}

impl From<String> for Value {
	fn from(v: String) -> Self {
		Value::Text(v)
	}
}

impl From<&String> for Value {
	fn from(v: &String) -> Self {
		Value::Text(v.clone())
	}
}

impl From<i64> for Value {
	fn from(v: i64) -> Self {
		Value::Integer(v)
	}
}

impl From<i32> for Value {
	fn from(v: i32) -> Self {
		Value::Integer(i64::from(v))
	}
}

impl From<u32> for Value {
	fn from(v: u32) -> Self {
		Value::Integer(i64::from(v))
	}
}

impl From<u64> for Value {
	/// Values above `i64::MAX` saturate (SQLite integers are signed 64-bit).
	fn from(v: u64) -> Self {
		Value::Integer(i64::try_from(v).unwrap_or(i64::MAX))
	}
}

impl From<f64> for Value {
	fn from(v: f64) -> Self {
		Value::Real(v)
	}
}

impl From<bool> for Value {
	fn from(v: bool) -> Self {
		Value::Integer(i64::from(v))
	}
}

impl From<Vec<u8>> for Value {
	fn from(v: Vec<u8>) -> Self {
		Value::Blob(v)
	}
}

impl<T: Into<Value>> From<Option<T>> for Value {
	fn from(v: Option<T>) -> Self {
		v.map_or(Value::Null, Into::into)
	}
}

/// Positional bindings: `params!["a.env", 3, None::<String>]`.
#[macro_export]
macro_rules! params {
	($($value:expr),* $(,)?) => {
		[$($crate::sql::Value::from($value)),*]
	};
}

pub use crate::params;

#[cfg(test)]
mod tests {
	use super::*;

	fn rows() -> Rows {
		Rows {
			columns: vec!["path".into(), "line".into(), "note".into(), "score".into()],
			values: vec![
				vec![
					Value::Text("a.env".into()),
					Value::Integer(3),
					Value::Null,
					Value::Real(1.5),
				],
				vec![
					Value::Text("b.pem".into()),
					Value::Real(7.9),
					Value::Text("x".into()),
					Value::Integer(2),
				],
			],
			rows_written: 0,
		}
	}

	#[test]
	fn rows_by_column_name() {
		let all = Row::all(rows());
		assert_eq!(all.len(), 2);
		assert_eq!(all[0].text("path"), Some("a.env"));
		assert_eq!(all[0].int("line"), Some(3));
		assert!(all[0].is_null("note"));
		assert!(all[0].is_null("missing"));
		assert_eq!(all[0].real("score"), Some(1.5));
		assert_eq!(all[1].int("line"), Some(7));
		assert_eq!(all[1].real("score"), Some(2.0));
		assert_eq!(all[1].text("line"), None);
		assert_eq!(all[1].columns().len(), 4);
	}

	#[test]
	fn params_convert_rust_values() {
		let p = params!["a", 3i64, 2u32, true, None::<String>, Some("x"), 1.5f64];
		assert!(matches!(&p[0], Value::Text(s) if s == "a"));
		assert!(matches!(p[1], Value::Integer(3)));
		assert!(matches!(p[2], Value::Integer(2)));
		assert!(matches!(p[3], Value::Integer(1)));
		assert!(matches!(p[4], Value::Null));
		assert!(matches!(&p[5], Value::Text(s) if s == "x"));
		assert!(matches!(p[6], Value::Real(r) if r == 1.5));
		assert!(matches!(Value::from(u64::MAX), Value::Integer(i64::MAX)));
	}
}
