//! `rawkode.kanban`: boards over a repository's issues.
//!
//! Installed on a repository beside Issues (the manifest `requires` it). It keeps a card for
//! every issue of the repository from the snapshots Issues emits (`x.rawkode.issues.issue.*`),
//! keeping the highest version of each, including issues created before it was installed.
//!
//! Boards come from the settings (`boards.<id>`), which Kanban's CUE module checks against the
//! repository's Issues properties: columns are a select property's options (all, or a subset in
//! a chosen order) or the issue's state; optional lanes (rows) and groups (headed groups inside
//! each cell) are a select, multi-select or person property, or the issue's project. A board may
//! show only one project's issues. Each board's properties are also checked against the ones the
//! installed Issues lists (its `properties` tool): a board bound to anything else is invalid.
//! Moving a card calls Issues' `update`; the manifest's `permissions.calls` allows both calls.
//! The card is then kept as the issue `update` returns it, unless a newer snapshot is already
//! kept; the snapshot of that version replaces it when it arrives.
//!
//! Tools: `list_boards`, `board` and `move`. The `board` slot renders a board (`props.board`),
//! and its `move` action moves a card.

pub mod board;

use board::{
    Board, Card, boards_from_settings, card_from_issue, card_json, issues_mismatch, update_args,
};
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
    let properties: Map<String, Value> = serde_json::from_str(&text(row, "properties")?)
        .map_err(|e| Error::internal(e.to_string()))?;
    Ok(Card {
        repo_id: text(row, "repo_id")?,
        number: row
            .int("number")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or_default(),
        title: text(row, "title")?,
        state: text(row, "state")?,
        project: row.text("project").map(str::to_owned),
        properties,
        version: row
            .int("version")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or_default(),
    })
}

const CARD_COLUMNS: &str = "repo_id, number, title, state, project, properties, version";

fn cards() -> Result<Vec<Card>, Error> {
    sql::query(
        &format!("SELECT {CARD_COLUMNS} FROM cards ORDER BY number"),
        &[],
    )?
    .iter()
    .map(card_from)
    .collect()
}

fn card(number: u32) -> Result<Option<Card>, Error> {
    sql::first(
        &format!("SELECT {CARD_COLUMNS} FROM cards WHERE number = ?"),
        &params![number],
    )?
    .map(|row| card_from(&row))
    .transpose()
}

/// Which kept card a new one replaces.
#[derive(Clone, Copy)]
enum Replace {
    /// An older version, or the same one: an issue's snapshot is the authority on its version.
    UpToSameVersion,
    /// Only an older version: a card built from a tool's result never replaces the snapshot
    /// of its version.
    OlderOnly,
}

/// Keeps `card` unless the kept card is newer, as `replace` says. `repo_path` is the `org/repo`
/// path of the repository, kept for reading the table by hand.
fn upsert(card: &Card, repo_path: &str, replace: Replace) -> Result<(), Error> {
    let properties = Value::Object(card.properties.clone()).to_string();
    let newer = match replace {
        Replace::UpToSameVersion => "excluded.version >= cards.version",
        Replace::OlderOnly => "excluded.version > cards.version",
    };
    sql::execute(
        &format!(
            "INSERT INTO cards (repo_id, repo_path, number, title, state, project, properties, version)
		 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		 ON CONFLICT (repo_id, number) DO UPDATE SET
		   repo_path = excluded.repo_path, title = excluded.title, state = excluded.state,
		   project = excluded.project, properties = excluded.properties,
		   version = excluded.version
		 WHERE {newer}"
        ),
        &params![
            card.repo_id.as_str(),
            repo_path,
            card.number,
            card.title.as_str(),
            card.state.as_str(),
            card.project.as_deref(),
            properties.as_str(),
            card.version
        ],
    )?;
    Ok(())
}

/// Boards, and the ids of definitions that are not boards with why.
type Boards = (Vec<Board>, Vec<(String, String)>);

/// The boards in the installation's settings on repository `repo`, and the definitions that are
/// not boards, with why. A board using a property that is not one of the installed Issues'
/// properties is not a board.
fn boards(repo: Option<&str>) -> Result<Boards, Error> {
    let (boards, mut invalid) = boards_from_settings(&tartan_ext::host::config());
    if !boards.iter().any(Board::uses_properties) {
        return Ok((boards, invalid));
    }
    let repo = repo.ok_or_else(|| Error::invalid("the call names no repository"))?;
    let listed = calls::call(ISSUES, Some(repo), "properties", &json!({}))?;
    let properties = listed["properties"].as_array().cloned().unwrap_or_default();
    let mut valid = Vec::with_capacity(boards.len());
    for board in boards {
        match issues_mismatch(&board, &properties) {
            Some(problem) => invalid.push((board.id.clone(), problem)),
            None => valid.push(board),
        }
    }
    invalid.sort();
    Ok((valid, invalid))
}

fn board_arg(args: &Value, repo: Option<&str>) -> Result<Board, Error> {
    let id = args["board"]
        .as_str()
        .ok_or_else(|| Error::invalid("board is required"))?;
    let (boards, invalid) = boards(repo)?;
    if let Some((_, problem)) = invalid.iter().find(|(invalid, _)| invalid == id) {
        return Err(Error::invalid(format!(
            "board {id} is not valid: {problem}"
        )));
    }
    boards
        .into_iter()
        .find(|board| board.id == id)
        .ok_or_else(|| Error::not_found(format!("board {id}")))
}

/// Moves a card: updates the issue through Issues, then keeps the card as the issue it returns.
fn move_card(args: &Value, repo: Option<&str>) -> Result<Value, Error> {
    let board = board_arg(args, repo)?;
    let number = args["number"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| Error::invalid("number is required"))?;
    let column = args["column"]
        .as_str()
        .ok_or_else(|| Error::invalid("column is required"))?;
    if !board.columns.iter().any(|c| c.value == column) {
        return Err(Error::invalid(format!(
            "{column} is not a column of board {}",
            board.id
        )));
    }
    let current = card(number)?
        .filter(|card| board.includes(card))
        .ok_or_else(|| Error::not_found(format!("card #{number} on board {}", board.id)))?;
    let updated = calls::call(
        ISSUES,
        Some(&current.repo_id),
        "update",
        &update_args(&board, number, column),
    )?;
    let card = card_from_issue(&current.repo_id, &updated["issue"])
        .ok_or_else(|| Error::internal("Issues' update returned no issue"))?;
    let repo_path = sql::first(
        "SELECT repo_path FROM cards WHERE number = ?",
        &params![number],
    )?
    .and_then(|row| row.text("repo_path").map(str::to_owned))
    .unwrap_or_default();
    upsert(&card, &repo_path, Replace::OlderOnly)?;
    Ok(json!({ "card": card_json(&card) }))
}

/// One card with a button for each other column of the board.
fn card_node(board: &Board, card: &Value, column: &str) -> ui::Node {
    let mut row = vec![ui::text(format!(
        "#{} {}",
        card["number"],
        card["title"].as_str().unwrap_or_default()
    ))];
    for other in board.columns.iter().filter(|c| c.value != column) {
        let payload = json!({ "board": board.id, "number": card["number"], "column": other.value });
        row.push(ui::button(
            &format!("→ {}", other.title),
            ui::action("move", Some(payload), None),
            None,
        ));
    }
    ui::card(row)
}

/// The `board` slot: a section per lane, each a grid of the columns, each cell its groups.
fn board_doc(board: &Board) -> Result<Value, Error> {
    let view = board.view(&cards()?);
    let width = u8::try_from(board.columns.len()).unwrap_or(u8::MAX);
    let titles: Vec<&str> = board.columns.iter().map(|c| c.title.as_str()).collect();
    let mut children = vec![ui::heading(&board.name, 2)];
    for lane in view["lanes"].as_array().into_iter().flatten() {
        let cells = lane["cells"]
            .as_array()
            .into_iter()
            .flatten()
            .zip(&titles)
            .map(|(cell, title)| {
                let column = cell["column"].as_str().unwrap_or_default();
                let mut items = vec![ui::heading(title, 3)];
                let mut empty = true;
                for group in cell["groups"].as_array().into_iter().flatten() {
                    if let Some(title) = group["title"].as_str().filter(|t| !t.is_empty()) {
                        items.push(ui::label(title));
                    }
                    for card in group["cards"].as_array().into_iter().flatten() {
                        empty = false;
                        items.push(card_node(board, card, column));
                    }
                }
                if empty {
                    items.push(ui::text("No issues"));
                }
                ui::stack(items)
            })
            .collect();
        let grid = ui::grid(cells, width);
        match lane["title"].as_str().filter(|t| !t.is_empty()) {
            Some(title) => children.push(ui::section(title, vec![grid])),
            None => children.push(grid),
        }
    }
    let unplaced = view["unplaced"].as_array().cloned().unwrap_or_default();
    if !unplaced.is_empty() {
        let first = board
            .columns
            .first()
            .map(|c| c.value.as_str())
            .unwrap_or_default();
        children.push(ui::section(
            "Unplaced",
            unplaced
                .iter()
                .map(|card| card_node(board, card, first))
                .collect(),
        ));
    }
    Ok(ui::doc(ui::stack(children)))
}

struct Kanban;

impl Extension for Kanban {
    fn on_event(event: Event) -> Result<(), Error> {
        if !event.kind.starts_with(SNAPSHOT_PREFIX) {
            return Ok(());
        }
        let envelope = parse(&event.data)?;
        let card = envelope["repo_id"]
            .as_str()
            .and_then(|repo_id| card_from_issue(repo_id, &envelope["data"]))
            .ok_or_else(|| {
                Error::invalid("an issue snapshot without repo_id, number or version")
            })?;
        let repo_path = envelope["path"]
            .as_str()
            .unwrap_or(&card.repo_id)
            .to_owned();
        upsert(&card, &repo_path, Replace::UpToSameVersion)
    }

    fn call_tool(name: String, args: Json, ctx: SlotContext) -> Result<Json, Error> {
        let args = parse(&args)?;
        let repo = ctx.repo.as_deref();
        let result = match name.as_str() {
            "list_boards" => {
                let (boards, invalid) = boards(repo)?;
                json!({
                    "boards": boards.iter().map(Board::summary).collect::<Vec<_>>(),
                    "invalid": invalid
                        .iter()
                        .map(|(id, problem)| json!({ "id": id, "problem": problem }))
                        .collect::<Vec<_>>(),
                })
            }
            "board" => board_arg(&args, repo)?.view(&cards()?),
            "move" => move_card(&args, repo)?,
            _ => return Err(Error::not_found(format!("tool {name}"))),
        };
        Ok(result.to_string())
    }

    fn render(slot: String, ctx: SlotContext, props: Json) -> Result<Json, Error> {
        if slot != "board" {
            return Err(Error::not_found(format!("slot {slot}")));
        }
        let board = board_arg(&parse(&props)?, ctx.repo.as_deref())?;
        Ok(board_doc(&board)?.to_string())
    }

    fn on_action(action: String, payload: Json, ctx: SlotContext) -> Result<Json, Error> {
        if action != "move" {
            return Err(Error::not_found(format!("action {action}")));
        }
        move_card(&parse(&payload)?, ctx.repo.as_deref())?;
        Ok(ui::result::refresh(&["board"]).to_string())
    }
}

export!(Kanban);
