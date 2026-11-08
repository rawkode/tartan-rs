//! `rawkode.kanban`: boards over the issues of an org's repositories.
//!
//! Installed once per org. It keeps a card for every issue in the org from the snapshots Issues
//! emits (`x.rawkode.issues.issue.*`), keeping the highest version of each. A board is a view
//! of those cards with its own columns: the org's, a repository's or a project's. Boards come
//! from the configuration: the org's settings (`boards`) and what repositories contribute
//! (`repos.<id>.settings.boards`, named `<org>/<repo>/<id>`). Moving a card calls Issues'
//! `update` in the card's repository, which the manifest's `permissions.calls` allows.
//!
//! Tools: `list_boards`, `board` and `move`.
//! The `board` slot renders a board (`props.board`), and its `move` action moves a card.

pub mod board;

use board::{Board, Card, boards_from_config, card_json, update_args};
use tartan_ext::serde_json::{self, Map, Value, json};
use tartan_ext::sql::{self, Row, params};
use tartan_ext::{Error, Event, Extension, Json, SlotContext, calls, export, ui};

/// The extension whose snapshots the cards follow.
const ISSUES: &str = "rawkode.issues";
/// The event types of those snapshots.
const SNAPSHOT_PREFIX: &str = "x.rawkode.issues.issue.";

fn parse(json: &str) -> Result<Value, Error> {
	serde_json::from_str(json).map_err(|e| Error::invalid(format!("not JSON: {e}")))
}

fn text(row: &Row, column: &str) -> Result<String, Error> {
	row.text(column)
		.map(str::to_owned)
		.ok_or_else(|| Error::internal(format!("column {column}")))
}

fn card_from(row: &Row) -> Result<Card, Error> {
	let fields: Map<String, Value> = serde_json::from_str(&text(row, "fields")?)
		.map_err(|e| Error::internal(e.to_string()))?;
	Ok(Card {
		repo_id: text(row, "repo_id")?,
		repo_path: text(row, "repo_path")?,
		number: row.int("number").and_then(|n| u32::try_from(n).ok()).unwrap_or_default(),
		title: text(row, "title")?,
		state: text(row, "state")?,
		project: row.text("project").map(str::to_owned),
		fields,
		version: row.int("version").and_then(|n| u32::try_from(n).ok()).unwrap_or_default(),
	})
}

const CARD_COLUMNS: &str = "repo_id, repo_path, number, title, state, project, fields, version";

fn cards() -> Result<Vec<Card>, Error> {
	sql::query(
		&format!("SELECT {CARD_COLUMNS} FROM cards ORDER BY repo_path, number"),
		&[],
	)?
	.iter()
	.map(card_from)
	.collect()
}

fn card(repo_id: &str, number: u32) -> Result<Option<Card>, Error> {
	sql::first(
		&format!("SELECT {CARD_COLUMNS} FROM cards WHERE repo_id = ? AND number = ?"),
		&params![repo_id, number],
	)?
	.map(|row| card_from(&row))
	.transpose()
}

/// Keeps `card` unless a newer version is already kept.
fn upsert(card: &Card) -> Result<(), Error> {
	let fields = Value::Object(card.fields.clone()).to_string();
	sql::execute(
		"INSERT INTO cards (repo_id, repo_path, number, title, state, project, fields, version)
		 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		 ON CONFLICT (repo_id, number) DO UPDATE SET
		   repo_path = excluded.repo_path, title = excluded.title, state = excluded.state,
		   project = excluded.project, fields = excluded.fields, version = excluded.version
		 WHERE excluded.version > cards.version",
		&params![
			card.repo_id.as_str(),
			card.repo_path.as_str(),
			card.number,
			card.title.as_str(),
			card.state.as_str(),
			card.project.as_deref(),
			fields.as_str(),
			card.version
		],
	)?;
	Ok(())
}

/// The boards in the installation's configuration, and the definitions that are not boards.
fn boards() -> (Vec<Board>, Vec<(String, String)>) {
	boards_from_config(&tartan_ext::host::config())
}

fn find_board(id: &str) -> Result<Board, Error> {
	boards()
		.0
		.into_iter()
		.find(|board| board.id == id)
		.ok_or_else(|| Error::not_found(format!("board {id}")))
}

/// Moves a card: updates the issue through Issues, then the card from the version it returns.
fn move_card(args: &Value) -> Result<Value, Error> {
	let board = find_board(args["board"].as_str().unwrap_or_default())?;
	let repo_id = args["repo_id"]
		.as_str()
		.ok_or_else(|| Error::invalid("repo_id is required"))?;
	let number = args["number"]
		.as_u64()
		.and_then(|n| u32::try_from(n).ok())
		.ok_or_else(|| Error::invalid("number is required"))?;
	let to = args["to"].as_str().ok_or_else(|| Error::invalid("to is required"))?;
	if !board.columns.iter().any(|c| c.value == to) {
		return Err(Error::invalid(format!("{to} is not a column of board {}", board.id)));
	}
	let mut card = card(repo_id, number)?
		.filter(|card| board.includes(card))
		.ok_or_else(|| Error::not_found(format!("card {repo_id}#{number} on board {}", board.id)))?;
	let updated = calls::call(ISSUES, Some(repo_id), "update", &update_args(&board, number, to))?;
	if board.field == "state" {
		card.state = to.to_owned();
	} else {
		card.fields.insert(board.field.clone(), json!(to));
	}
	if let Some(version) = updated["issue"]["version"].as_u64().and_then(|v| u32::try_from(v).ok()) {
		card.version = version;
	}
	upsert(&card)?;
	Ok(json!({ "card": card_json(&card) }))
}

fn board_doc(board: &Board) -> Result<Value, Error> {
	let view = board.view(&cards()?);
	let columns = view["columns"].as_array().cloned().unwrap_or_default();
	let lanes = columns
		.iter()
		.map(|column| {
			let value = column["value"].as_str().unwrap_or_default();
			let mut children = vec![ui::heading(column["title"].as_str().unwrap_or(value), 3)];
			let cards = column["cards"].as_array().cloned().unwrap_or_default();
			if cards.is_empty() {
				children.push(ui::text("No issues"));
			}
			for card in cards {
				let mut row = vec![ui::text(format!(
					"{}#{} {}",
					card["repo"].as_str().unwrap_or_default(),
					card["number"],
					card["title"].as_str().unwrap_or_default()
				))];
				for other in board.columns.iter().filter(|c| c.value != value) {
					let payload = json!({
						"board": board.id,
						"repo_id": card["repo_id"],
						"number": card["number"],
						"to": other.value,
					});
					row.push(ui::button(
						&format!("→ {}", other.title),
						ui::action("move", Some(payload), None),
						None,
					));
				}
				children.push(ui::card(row));
			}
			ui::stack(children)
		})
		.collect();
	Ok(ui::doc(ui::stack(vec![
		ui::heading(&board.name, 2),
		ui::grid(lanes, u8::try_from(board.columns.len()).unwrap_or(1)),
	])))
}

struct Kanban;

impl Extension for Kanban {
	fn on_event(event: Event) -> Result<(), Error> {
		if !event.kind.starts_with(SNAPSHOT_PREFIX) {
			return Ok(());
		}
		let envelope = parse(&event.data)?;
		let issue = &envelope["data"];
		let (Some(repo_id), Some(number), Some(version)) = (
			envelope["repo_id"].as_str(),
			issue["number"].as_u64().and_then(|n| u32::try_from(n).ok()),
			issue["version"].as_u64().and_then(|n| u32::try_from(n).ok()),
		) else {
			return Err(Error::invalid("an issue snapshot without repo_id, number or version"));
		};
		upsert(&Card {
			repo_id: repo_id.to_owned(),
			repo_path: envelope["path"].as_str().unwrap_or(repo_id).to_owned(),
			number,
			title: issue["title"].as_str().unwrap_or_default().to_owned(),
			state: issue["state"].as_str().unwrap_or("open").to_owned(),
			project: issue["project"].as_str().map(str::to_owned),
			fields: issue["fields"].as_object().cloned().unwrap_or_default(),
			version,
		})
	}

	fn call_tool(name: String, args: Json, _ctx: SlotContext) -> Result<Json, Error> {
		let args = parse(&args)?;
		let result = match name.as_str() {
			"list_boards" => {
				let (boards, invalid) = boards();
				json!({
					"boards": boards.iter().map(Board::to_json).collect::<Vec<_>>(),
					"invalid": invalid
						.iter()
						.map(|(id, problem)| json!({ "id": id, "problem": problem }))
						.collect::<Vec<_>>(),
				})
			}
			"board" => find_board(args["id"].as_str().unwrap_or_default())?.view(&cards()?),
			"move" => move_card(&args)?,
			_ => return Err(Error::not_found(format!("tool {name}"))),
		};
		Ok(result.to_string())
	}

	fn render(slot: String, _ctx: SlotContext, props: Json) -> Result<Json, Error> {
		if slot != "board" {
			return Err(Error::not_found(format!("slot {slot}")));
		}
		let props = parse(&props)?;
		let board = find_board(props["board"].as_str().unwrap_or_default())?;
		Ok(board_doc(&board)?.to_string())
	}

	fn on_action(action: String, payload: Json, _ctx: SlotContext) -> Result<Json, Error> {
		if action != "move" {
			return Err(Error::not_found(format!("action {action}")));
		}
		move_card(&parse(&payload)?)?;
		Ok(ui::result::refresh(&["board"]).to_string())
	}
}

export!(Kanban);
