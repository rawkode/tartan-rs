//! The `issues` table (`migrations/`). Reads also run in the
//! read-only render; writes only from tools and the tab's action.

use tartan_ext::{effects, host};
use tartan_ext::sql::{self, params, Row, Value};
use tartan_ext::Error;

use tartan_ext::serde_json::{self, Map};

use crate::issue::{Author, Change, Filter, Issue, NewIssue, State};

const COLUMNS: &str =
	"number, title, body, project, state, author_kind, author_id, on_behalf_of, created_at, updated_at, closed_at, fields, version";

fn time(row: &Row, column: &str) -> Option<u64> {
	row.int(column).and_then(|t| u64::try_from(t).ok())
}

fn issue(row: &Row) -> Option<Issue> {
	Some(Issue {
		number: u32::try_from(row.int("number")?).ok()?,
		title: row.text("title")?.to_string(),
		body: row.text("body").unwrap_or("").to_string(),
		project: row.text("project").map(str::to_string),
		state: State::parse(row.text("state")?)?,
		author: Author {
			kind: row.text("author_kind")?.to_string(),
			id: row.text("author_id")?.to_string(),
			on_behalf_of: row.text("on_behalf_of").map(str::to_string),
		},
		created_at: time(row, "created_at").unwrap_or(0),
		updated_at: time(row, "updated_at").unwrap_or(0),
		closed_at: time(row, "closed_at"),
		fields: row
			.text("fields")
			.and_then(|t| serde_json::from_str(t).ok())
			.unwrap_or_default(),
		version: row
			.int("version")
			.and_then(|v| u32::try_from(v).ok())
			.unwrap_or(1),
	})
}

/// Opens an issue with the repository's next number.
pub fn create(new: &NewIssue, project: Option<&str>, author: &Author) -> Result<Issue, Error> {
	let fields = serde_json::to_string(&new.fields).map_err(|e| Error::internal(e.to_string()))?;
	let at = host::now_ms();
	let next = sql::first("SELECT COALESCE(MAX(number), 0) + 1 AS n FROM issues", &[])?
		.and_then(|r| r.int("n"))
		.and_then(|n| u32::try_from(n).ok())
		.ok_or_else(|| Error::internal("no next issue number"))?;
	sql::execute(
		"INSERT INTO issues (number, title, body, project, state, author_kind, author_id, on_behalf_of, created_at, updated_at, fields, version) VALUES (?, ?, ?, ?, 'open', ?, ?, ?, ?, ?, ?, 1)",
		&params![
			next,
			new.title.as_str(),
			new.body.as_str(),
			project,
			author.kind.as_str(),
			author.id.as_str(),
			author.on_behalf_of.as_deref(),
			at,
			at,
			fields.as_str()
		],
	)?;
	let created = Issue {
		number: next,
		title: new.title.clone(),
		body: new.body.clone(),
		project: project.map(str::to_string),
		state: State::Open,
		author: author.clone(),
		created_at: at,
		updated_at: at,
		closed_at: None,
		fields: new.fields.clone(),
		version: 1,
	};
	emit_snapshot("issue.created", &created)?;
	Ok(created)
}

/// Emits `x.rawkode.issues.<kind>` with the issue as it now is. Every write emits one, so a
/// consumer can keep a copy of each issue by keeping the snapshot with the highest `version`;
/// the body is left out.
fn emit_snapshot(kind: &str, issue: &Issue) -> Result<(), Error> {
	let snapshot = serde_json::json!({
		"number": issue.number,
		"title": issue.title,
		"project": issue.project,
		"state": issue.state.as_str(),
		"fields": issue.fields,
		"version": issue.version,
		"updated_at": issue.updated_at,
	});
	effects::emit(kind, None, &snapshot)
}

pub fn get(number: u32) -> Result<Option<Issue>, Error> {
	let row = sql::first(
		&format!("SELECT {COLUMNS} FROM issues WHERE number = ?"),
		&params![number],
	)?;
	Ok(row.as_ref().and_then(issue))
}

/// The newest issues first, of one project when given.
pub fn list(
	filter: Filter,
	project: Option<&str>,
	before: Option<u32>,
	limit: u32,
) -> Result<Vec<Issue>, Error> {
	let mut conditions = Vec::new();
	let mut bound = Vec::new();
	if let Filter::Only(state) = filter {
		conditions.push("state = ?");
		bound.push(Value::from(state.as_str()));
	}
	if let Some(root) = project {
		conditions.push("project = ?");
		bound.push(Value::from(root));
	}
	if let Some(number) = before {
		conditions.push("number < ?");
		bound.push(Value::from(number));
	}
	let filtered = if conditions.is_empty() {
		String::new()
	} else {
		format!("WHERE {}", conditions.join(" AND "))
	};
	bound.push(Value::from(limit));
	let rows = sql::query(
		&format!("SELECT {COLUMNS} FROM issues {filtered} ORDER BY number DESC LIMIT ?"),
		&bound,
	)?;
	Ok(rows.iter().filter_map(issue).collect())
}

/// `(open, closed)`, of one project when given.
pub fn counts(project: Option<&str>) -> Result<(i64, i64), Error> {
	const COUNTS: &str = "SELECT COALESCE(SUM(CASE WHEN state = 'open' THEN 1 ELSE 0 END), 0) AS open, COALESCE(SUM(CASE WHEN state = 'closed' THEN 1 ELSE 0 END), 0) AS closed FROM issues";
	let row = match project {
		None => sql::first(COUNTS, &[])?,
		Some(root) => sql::first(&format!("{COUNTS} WHERE project = ?"), &params![root])?,
	};
	Ok(row.map_or((0, 0), |r| {
		(r.int("open").unwrap_or(0), r.int("closed").unwrap_or(0))
	}))
}

/// Applies one change in a single write: `(issue, changed)`, or None when there is
/// no such issue. A change that alters nothing writes nothing. The caller holds
/// the extension's write lock, so reading the version first is safe.
pub fn apply(number: u32, change: &Change) -> Result<Option<(Issue, bool)>, Error> {
	let Some(current) = get(number)? else {
		return Ok(None);
	};
	if let Some(expected) = change.expected_version {
		if expected != current.version {
			return Err(Error::conflict(format!(
				"issue #{number} changed: it is at version {}",
				current.version
			)));
		}
	}
	let mut fields: Map<String, serde_json::Value> = current.fields.clone();
	for (name, value) in &change.fields {
		if value.is_null() {
			fields.remove(name);
		} else {
			fields.insert(name.clone(), value.clone());
		}
	}
	let next = Issue {
		title: change.title.clone().unwrap_or_else(|| current.title.clone()),
		body: change.body.clone().unwrap_or_else(|| current.body.clone()),
		state: change.state.unwrap_or(current.state),
		fields,
		..current.clone()
	};
	if next == current {
		return Ok(Some((current, false)));
	}
	let at = host::now_ms();
	let closed_at = match (current.state, next.state) {
		(State::Open, State::Closed) => Some(at),
		(State::Closed, State::Open) => None,
		_ => current.closed_at,
	};
	let version = current.version + 1;
	let encoded = serde_json::to_string(&next.fields).map_err(|e| Error::internal(e.to_string()))?;
	sql::execute(
		"UPDATE issues SET title = ?, body = ?, state = ?, fields = ?, version = ?, updated_at = ?, closed_at = ? WHERE number = ?",
		&params![
			next.title.as_str(),
			next.body.as_str(),
			next.state.as_str(),
			encoded.as_str(),
			version,
			at,
			closed_at,
			number
		],
	)?;
	let updated = Issue {
		version,
		updated_at: at,
		closed_at,
		..next
	};
	emit_snapshot("issue.updated", &updated)?;
	Ok(Some((updated, true)))
}
