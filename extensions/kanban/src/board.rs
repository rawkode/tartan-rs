//! Boards and cards, independent of storage.

use tartan_ext::Error;
use tartan_ext::serde_json::{Map, Value, json};

/// The most columns a board has.
pub const COLUMNS_MAX: usize = 12;

/// What a board shows: every card in the org, a repository's, or one project's in a repository.
/// A repository is named by its id or its `org/repo` path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
	Org,
	Repo { repo: String },
	Project { repo: String, project: String },
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
	/// A board from its definition in the configuration. `scope` fixes the scope for a board a
	/// repository contributes; the org's boards set their own (`scope: {kind, repo, project}`).
	pub fn from_definition(id: &str, definition: &Value, scope: Option<Scope>) -> Result<Self, Error> {
		let name = definition["name"].as_str().unwrap_or(id).trim();
		if name.is_empty() || name.chars().count() > 100 {
			return Err(Error::invalid("name is 1 to 100 characters"));
		}
		let field = definition["field"].as_str().unwrap_or("state").trim();
		if field.is_empty() || field.len() > 64 {
			return Err(Error::invalid("field names an issue field, or state"));
		}
		let scope = match scope {
			Some(Scope::Repo { repo }) => match definition["project"].as_str() {
				Some(project) if !project.is_empty() => Scope::Project {
					repo,
					project: project.to_owned(),
				},
				_ => Scope::Repo { repo },
			},
			Some(fixed) => fixed,
			None => scope_from(&definition["scope"])?,
		};
		Ok(Self {
			id: id.to_owned(),
			name: name.to_owned(),
			scope,
			field: field.to_owned(),
			columns: columns_from(&definition["columns"])?,
		})
	}

	/// Whether `card` is on this board.
	pub fn includes(&self, card: &Card) -> bool {
		match &self.scope {
			Scope::Org => true,
			Scope::Repo { repo } => names(card, repo),
			Scope::Project { repo, project } => {
				names(card, repo) && card.project.as_deref() == Some(project.as_str())
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
			Scope::Repo { repo } => json!({ "kind": "repo", "repo": repo }),
			Scope::Project { repo, project } => {
				json!({ "kind": "project", "repo": repo, "project": project })
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

/// Whether `repo` (an id or an `org/repo` path) names `card`'s repository.
fn names(card: &Card, repo: &str) -> bool {
	card.repo_id == repo || card.repo_path == repo
}

/// Whether `id` can name a board: 1 to 64 lowercase letters, digits and `-`.
pub fn valid_id(id: &str) -> bool {
	!id.is_empty()
		&& id.len() <= 64
		&& id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The boards in Kanban's configuration: the org's own (`boards`), then each repository's
/// contribution (`repos.<id>.settings.boards`), named `<org>/<repo>/<id>`. Definitions that are
/// not boards are returned with their problem.
pub fn boards_from_config(config: &Value) -> (Vec<Board>, Vec<(String, String)>) {
	let mut boards = Vec::new();
	let mut invalid = Vec::new();
	let mut add = |id: String, definition: &Value, scope: Option<Scope>, local: &str| {
		if !valid_id(local) {
			invalid.push((id, "a board id is 1 to 64 lowercase letters, digits and -".to_owned()));
			return;
		}
		match Board::from_definition(&id, definition, scope) {
			Ok(board) => boards.push(board),
			Err(error) => invalid.push((id, error_text(&error))),
		}
	};
	if let Some(own) = config["boards"].as_object() {
		for (id, definition) in own {
			add(id.clone(), definition, None, id);
		}
	}
	if let Some(repos) = config["repos"].as_object() {
		for (repo_id, contribution) in repos {
			let path = contribution["path"].as_str().unwrap_or(repo_id);
			if let Some(contributed) = contribution["settings"]["boards"].as_object() {
				for (id, definition) in contributed {
					let scope = Scope::Repo { repo: repo_id.clone() };
					add(format!("{path}/{id}"), definition, Some(scope), id);
				}
			}
		}
	}
	(boards, invalid)
}

fn error_text(error: &Error) -> String {
	match error {
		Error::Denied(m)
		| Error::NotFound(m)
		| Error::Invalid(m)
		| Error::Conflict(m)
		| Error::Unavailable(m)
		| Error::Internal(m) => m.clone(),
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
		"repo" => Ok(Scope::Repo { repo: text("repo")? }),
		"project" => Ok(Scope::Project {
			repo: text("repo")?,
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
			repo_id: format!("id-{repo}"),
			repo_path: format!("acme/{repo}"),
			number,
			title: format!("#{number}"),
			state: "open".to_owned(),
			project: project.map(str::to_owned),
			fields,
			version: 1,
		}
	}

	fn columns() -> Value {
		json!([{"value": "todo"}, {"value": "doing"}, {"value": "done"}])
	}

	#[test]
	fn boards_come_from_the_org_and_from_repositories() {
		let config = json!({
			"boards": {
				"all": {"field": "status", "columns": columns()},
				"web": {"field": "status", "columns": columns(),
						"scope": {"kind": "repo", "repo": "acme/web"}},
				"bad": {"columns": []},
			},
			"repos": {
				"id-api": {"path": "acme/api", "settings": {"boards": {
					"api": {"field": "status", "columns": columns()},
					"lsp": {"field": "status", "columns": columns(), "project": "crates/lsp"},
					"Bad": {"columns": columns()},
				}}},
			},
		});
		let (boards, invalid) = boards_from_config(&config);
		let ids: Vec<&str> = boards.iter().map(|b| b.id.as_str()).collect();
		assert_eq!(ids, vec!["all", "web", "acme/api/api", "acme/api/lsp"]);
		let bad: Vec<&str> = invalid.iter().map(|(id, _)| id.as_str()).collect();
		assert_eq!(bad, vec!["bad", "acme/api/Bad"]);

		let cards = [
			card("api", 1, Some("crates/lsp"), Some("doing")),
			card("api", 2, None, None),
			card("web", 1, None, Some("done")),
		];
		let count = |id: &str| {
			let board = boards.iter().find(|b| b.id == id).expect("a board");
			cards.iter().filter(|c| board.includes(c)).count()
		};
		assert_eq!(count("all"), 3);
		assert_eq!(count("web"), 1);
		assert_eq!(count("acme/api/api"), 2);
		assert_eq!(count("acme/api/lsp"), 1);
	}

	#[test]
	fn cards_without_a_value_start_in_the_first_column() {
		let board = Board::from_definition(
			"b",
			&json!({"field": "status", "columns": columns()}),
			None,
		)
		.expect("a board");
		let view = board.view(&[
			card("api", 1, None, None),
			card("api", 2, None, Some("doing")),
			card("api", 3, None, Some("blocked")),
		]);
		assert_eq!(view["columns"][0]["cards"].as_array().map(Vec::len), Some(1));
		assert_eq!(view["columns"][1]["cards"].as_array().map(Vec::len), Some(1));
		assert_eq!(view["unplaced"].as_array().map(Vec::len), Some(1));
	}

	#[test]
	fn refuses_bad_definitions() {
		let bad = [
			json!({"columns": []}),
			json!({"columns": [{"value": "a"}, {"value": "a"}]}),
			json!({"columns": [{"value": "a"}], "scope": {"kind": "repo"}}),
			json!({"columns": [{"value": "a"}], "scope": {"kind": "team"}}),
		];
		for definition in bad {
			assert!(Board::from_definition("b", &definition, None).is_err(), "{definition}");
		}
	}

	#[test]
	fn moves_update_the_board_field() {
		let status = Board::from_definition(
			"b",
			&json!({"field": "status", "columns": columns()}),
			None,
		)
		.expect("a board");
		assert_eq!(
			update_args(&status, 3, "done"),
			json!({"number": 3, "fields": {"status": "done"}})
		);
		let mut state = status.clone();
		state.field = "state".to_owned();
		assert_eq!(update_args(&state, 3, "closed"), json!({"number": 3, "state": "closed"}));
	}
}
