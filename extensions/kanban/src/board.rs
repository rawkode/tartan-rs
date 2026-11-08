//! Boards and cards, independent of storage.

use tartan_ext::Error;
use tartan_ext::serde_json::{Map, Value, json};

/// The most columns a board has.
pub const COLUMNS_MAX: usize = 12;

/// What a board shows: every card in the org, a repository's, or one project's in a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
	Org,
	Repo { repo_id: String },
	Project { repo_id: String, project: String },
}

/// One column: the field value it holds, and its title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
	pub value: String,
	pub title: String,
}

/// A board's definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Board {
	pub id: String,
	pub name: String,
	pub scope: Scope,
	/// The issue field the columns are values of; `state` is the issue's open or closed.
	pub field: String,
	pub columns: Vec<Column>,
}

/// An issue as the board keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
	pub repo_id: String,
	pub repo_path: String,
	pub number: u32,
	pub title: String,
	pub state: String,
	pub project: Option<String>,
	pub fields: Map<String, Value>,
	pub version: u32,
}

impl Board {
	/// Parses a board from tool arguments, checking every rule.
	pub fn from_args(args: &Value) -> Result<Self, Error> {
		let id = args["id"].as_str().unwrap_or_default();
		let valid_id = !id.is_empty()
			&& id.len() <= 64
			&& id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
		if !valid_id {
			return Err(Error::invalid("id is 1 to 64 lowercase letters, digits and -"));
		}
		let name = args["name"].as_str().unwrap_or(id).trim();
		if name.is_empty() || name.chars().count() > 100 {
			return Err(Error::invalid("name is 1 to 100 characters"));
		}
		let field = args["field"].as_str().unwrap_or("state").trim();
		if field.is_empty() || field.len() > 64 {
			return Err(Error::invalid("field names an issue field, or state"));
		}
		let columns = columns_from(&args["columns"])?;
		Ok(Self {
			id: id.to_owned(),
			name: name.to_owned(),
			scope: scope_from(&args["scope"])?,
			field: field.to_owned(),
			columns,
		})
	}

	/// Whether `card` is on this board.
	pub fn includes(&self, card: &Card) -> bool {
		match &self.scope {
			Scope::Org => true,
			Scope::Repo { repo_id } => card.repo_id == *repo_id,
			Scope::Project { repo_id, project } => {
				card.repo_id == *repo_id && card.project.as_deref() == Some(project.as_str())
			}
		}
	}

	/// The column value of `card`: the field's value, or the first column's when the issue has
	/// none yet (a field's default is not part of the snapshot).
	pub fn value_of(&self, card: &Card) -> Option<String> {
		let value = if self.field == "state" {
			Some(card.state.clone())
		} else {
			card.fields.get(&self.field).and_then(|v| v.as_str()).map(str::to_owned)
		};
		value.or_else(|| self.columns.first().map(|c| c.value.clone()))
	}

	/// The board with its cards in columns; cards whose value is no column's are `unplaced`.
	pub fn view(&self, cards: &[Card]) -> Value {
		let mut columns: Vec<(Column, Vec<Value>)> =
			self.columns.iter().map(|c| (c.clone(), Vec::new())).collect();
		let mut unplaced = Vec::new();
		for card in cards.iter().filter(|card| self.includes(card)) {
			let value = self.value_of(card);
			let column = columns.iter_mut().find(|(c, _)| Some(&c.value) == value.as_ref());
			match column {
				Some((_, list)) => list.push(card_json(card)),
				None => unplaced.push(card_json(card)),
			}
		}
		json!({
			"board": self.to_json(),
			"columns": columns
				.into_iter()
				.map(|(c, cards)| json!({ "value": c.value, "title": c.title, "cards": cards }))
				.collect::<Vec<_>>(),
			"unplaced": unplaced,
		})
	}

	pub fn to_json(&self) -> Value {
		let scope = match &self.scope {
			Scope::Org => json!({ "kind": "org" }),
			Scope::Repo { repo_id } => json!({ "kind": "repo", "repo_id": repo_id }),
			Scope::Project { repo_id, project } => {
				json!({ "kind": "project", "repo_id": repo_id, "project": project })
			}
		};
		json!({
			"id": self.id,
			"name": self.name,
			"scope": scope,
			"field": self.field,
			"columns": self.columns.iter()
				.map(|c| json!({ "value": c.value, "title": c.title }))
				.collect::<Vec<_>>(),
		})
	}
}

/// The arguments of Issues' `update` that move a card to `value`.
pub fn update_args(board: &Board, number: u32, value: &str) -> Value {
	if board.field == "state" {
		json!({ "number": number, "state": value })
	} else {
		json!({ "number": number, "fields": { board.field.as_str(): value } })
	}
}

pub fn card_json(card: &Card) -> Value {
	json!({
		"repo_id": card.repo_id,
		"repo": card.repo_path,
		"number": card.number,
		"title": card.title,
		"state": card.state,
		"project": card.project,
		"fields": card.fields,
	})
}

fn scope_from(scope: &Value) -> Result<Scope, Error> {
	let text = |key: &str| {
		scope[key]
			.as_str()
			.filter(|s| !s.is_empty())
			.map(str::to_owned)
			.ok_or_else(|| Error::invalid(format!("scope.{key} is required")))
	};
	match scope["kind"].as_str().unwrap_or("org") {
		"org" => Ok(Scope::Org),
		"repo" => Ok(Scope::Repo { repo_id: text("repo_id")? }),
		"project" => Ok(Scope::Project {
			repo_id: text("repo_id")?,
			project: text("project")?,
		}),
		other => Err(Error::invalid(format!("scope.kind {other:?} is org, repo or project"))),
	}
}

fn columns_from(columns: &Value) -> Result<Vec<Column>, Error> {
	let list = columns
		.as_array()
		.filter(|list| !list.is_empty() && list.len() <= COLUMNS_MAX)
		.ok_or_else(|| Error::invalid(format!("columns is 1 to {COLUMNS_MAX} columns")))?;
	let mut out: Vec<Column> = Vec::with_capacity(list.len());
	for column in list {
		let value = column["value"]
			.as_str()
			.filter(|v| !v.is_empty())
			.ok_or_else(|| Error::invalid("each column has a value"))?;
		if out.iter().any(|c| c.value == value) {
			return Err(Error::invalid(format!("column {value} is repeated")));
		}
		let title = column["title"].as_str().unwrap_or(value);
		out.push(Column {
			value: value.to_owned(),
			title: title.to_owned(),
		});
	}
	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn card(repo: &str, number: u32, project: Option<&str>, status: Option<&str>) -> Card {
		let mut fields = Map::new();
		if let Some(status) = status {
			fields.insert("status".to_owned(), json!(status));
		}
		Card {
			repo_id: repo.to_owned(),
			repo_path: format!("acme/{repo}"),
			number,
			title: format!("#{number}"),
			state: "open".to_owned(),
			project: project.map(str::to_owned),
			fields,
			version: 1,
		}
	}

	fn board(scope: Value) -> Board {
		Board::from_args(&json!({
			"id": "b", "field": "status", "scope": scope,
			"columns": [{"value": "todo"}, {"value": "doing"}, {"value": "done"}],
		}))
		.expect("valid board")
	}

	#[test]
	fn scopes_filter_cards() {
		let cards = [
			card("api", 1, Some("crates/lsp"), Some("doing")),
			card("api", 2, None, None),
			card("web", 1, None, Some("done")),
		];
		let count = |b: &Board| cards.iter().filter(|c| b.includes(c)).count();
		assert_eq!(count(&board(json!({"kind": "org"}))), 3);
		assert_eq!(count(&board(json!({"kind": "repo", "repo_id": "api"}))), 2);
		let project = json!({"kind": "project", "repo_id": "api", "project": "crates/lsp"});
		assert_eq!(count(&board(project)), 1);
	}

	#[test]
	fn cards_without_a_value_start_in_the_first_column() {
		let view = board(json!({"kind": "org"})).view(&[
			card("api", 1, None, None),
			card("api", 2, None, Some("doing")),
			card("api", 3, None, Some("blocked")),
		]);
		assert_eq!(view["columns"][0]["cards"].as_array().map(Vec::len), Some(1));
		assert_eq!(view["columns"][1]["cards"].as_array().map(Vec::len), Some(1));
		assert_eq!(view["unplaced"].as_array().map(Vec::len), Some(1));
	}

	#[test]
	fn refuses_bad_boards() {
		let bad = [
			json!({"id": "Bad", "columns": [{"value": "a"}]}),
			json!({"id": "b", "columns": []}),
			json!({"id": "b", "columns": [{"value": "a"}, {"value": "a"}]}),
			json!({"id": "b", "columns": [{"value": "a"}], "scope": {"kind": "repo"}}),
			json!({"id": "b", "columns": [{"value": "a"}], "scope": {"kind": "team"}}),
		];
		for args in bad {
			assert!(Board::from_args(&args).is_err(), "{args}");
		}
	}

	#[test]
	fn moves_update_the_board_field() {
		let status = board(json!({"kind": "org"}));
		assert_eq!(
			update_args(&status, 3, "done"),
			json!({"number": 3, "fields": {"status": "done"}})
		);
		let mut state = status.clone();
		state.field = "state".to_owned();
		assert_eq!(update_args(&state, 3, "closed"), json!({"number": 3, "state": "closed"}));
	}
}
