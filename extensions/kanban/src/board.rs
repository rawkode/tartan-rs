//! Boards and cards, independent of storage.
//!
//! A board comes from Kanban's settings as `cue export` gives them. Kanban's CUE
//! module has already checked each board against the repository's Issues
//! properties, so a property reference arrives as the property itself (its
//! `name`, `kind`, `options`, `labels` and `default`). The checks here only
//! keep a hand-written configuration from producing a broken view.

use tartan_ext::Error;
use tartan_ext::serde_json::{Map, Value, json};

/// The title of the lane or group of cards without a value.
pub const NONE_TITLE: &str = "None";

/// An Issues property a board uses, as the configuration carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub kind: String,
    pub options: Vec<String>,
    /// Titles of options, by option.
    pub labels: Map<String, Value>,
    pub default: Option<Value>,
}

impl Property {
    fn from_value(value: &Value) -> Result<Self, Error> {
        let name = value["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| Error::invalid("a property reference has no name"))?;
        let kind = value["kind"]
            .as_str()
            .ok_or_else(|| Error::invalid(format!("property {name} has no kind")))?;
        Ok(Self {
            name: name.to_owned(),
            kind: kind.to_owned(),
            options: value["options"]
                .as_array()
                .map(|o| {
                    o.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            labels: value["labels"].as_object().cloned().unwrap_or_default(),
            default: value.get("default").filter(|v| !v.is_null()).cloned(),
        })
    }

    /// The title of a value: its label, or the value itself.
    pub fn title(&self, value: &str) -> String {
        self.labels
            .get(value)
            .and_then(Value::as_str)
            .unwrap_or(value)
            .to_owned()
    }

    /// The card's value of this property, or the property's default when the issue never set it.
    fn value<'a>(&'a self, card: &'a Card) -> Option<&'a Value> {
        card.properties
            .get(&self.name)
            .filter(|v| !v.is_null())
            .or(self.default.as_ref())
    }
}

/// What a board's columns are values of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnsBy {
    /// The issue's state, open or closed.
    State,
    /// A select property.
    Property(Property),
}

/// What a board's lanes, or the groups inside its cells, are values of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum By {
    /// The issue's project.
    Project,
    /// A select, multi-select or person property.
    Property(Property),
}

impl By {
    fn from_value(value: &Value, what: &str) -> Result<Self, Error> {
        if value.as_str() == Some("project") {
            return Ok(Self::Project);
        }
        let property = Property::from_value(value)?;
        match property.kind.as_str() {
            "select" | "multiselect" | "person" => Ok(Self::Property(property)),
            kind => Err(Error::invalid(format!(
                "{what} is a select, multi-select or person property, or \"project\", not a {kind}"
            ))),
        }
    }

    /// What the lanes or groups are by: `project` or the property's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Project => "project",
            Self::Property(p) => &p.name,
        }
    }

    /// The card's values: one per multi-select option, or None when it has none.
    fn values(&self, card: &Card) -> Vec<Option<String>> {
        let values: Vec<Option<String>> = match self {
            Self::Project => vec![card.project.clone()],
            Self::Property(p) => match p.value(card) {
                Some(Value::String(s)) => vec![Some(s.clone())],
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| Some(s.to_owned()))
                    .collect(),
                _ => Vec::new(),
            },
        };
        if values.is_empty() {
            vec![None]
        } else {
            values
        }
    }

    /// The buckets for `cards` in order: a select's options first (every one when `all_options`),
    /// then other values in sorted order, then None when a card has no value.
    fn buckets(&self, cards: &[&Card], all_options: bool) -> Vec<Bucket> {
        let mut present: Vec<Option<String>> = Vec::new();
        for card in cards {
            for value in self.values(card) {
                if !present.contains(&value) {
                    present.push(value);
                }
            }
        }
        let options: &[String] = match self {
            Self::Property(p) if p.kind != "person" => &p.options,
            _ => &[],
        };
        let mut order: Vec<String> = options
            .iter()
            .filter(|o| all_options || present.contains(&Some((*o).clone())))
            .cloned()
            .collect();
        let mut others: Vec<String> = present
            .iter()
            .flatten()
            .filter(|v| !options.contains(v))
            .cloned()
            .collect();
        others.sort();
        order.extend(others);
        let mut buckets: Vec<Bucket> = order
            .into_iter()
            .map(|value| Bucket {
                title: match self {
                    Self::Property(p) => p.title(&value),
                    Self::Project => value.clone(),
                },
                value: Some(value),
            })
            .collect();
        if present.contains(&None) {
            buckets.push(Bucket {
                value: None,
                title: NONE_TITLE.to_owned(),
            });
        }
        buckets
    }
}

/// A lane or a group: a value and its title; None holds cards without a value.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bucket {
    value: Option<String>,
    title: String,
}

/// One column: the value it holds, and its title.
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
    /// Only this project's issues, by its root path.
    pub project: Option<String>,
    pub columns_by: ColumnsBy,
    pub columns: Vec<Column>,
    pub lanes: Option<By>,
    pub group: Option<By>,
}

/// An issue as the board keeps it, from Issues' snapshots.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub repo_id: String,
    pub number: u32,
    pub title: String,
    pub state: String,
    pub project: Option<String>,
    pub properties: Map<String, Value>,
    pub version: u32,
}

impl Board {
    /// A board from its definition in the settings.
    pub fn from_definition(id: &str, definition: &Value) -> Result<Self, Error> {
        let name = definition["name"].as_str().unwrap_or(id).trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(Error::invalid("name is 1 to 100 characters"));
        }
        let project = match &definition["project"] {
            Value::Null => None,
            Value::String(p) if !p.is_empty() => Some(p.clone()),
            _ => return Err(Error::invalid("project is a root path, such as crates/lsp")),
        };
        let (columns_by, columns) = columns_from(&definition["columns"])?;
        let by = |key: &str| match &definition[key] {
            Value::Null => Ok(None),
            value => By::from_value(value, key).map(Some),
        };
        Ok(Self {
            id: id.to_owned(),
            name: name.to_owned(),
            project,
            columns_by,
            columns,
            lanes: by("lanes")?,
            group: by("group")?,
        })
    }

    /// Whether `card` is on this board.
    pub fn includes(&self, card: &Card) -> bool {
        self.project
            .as_ref()
            .is_none_or(|p| card.project.as_ref() == Some(p))
    }

    /// The column value of `card`: its value, the property's default, or else the first column.
    pub fn column_of(&self, card: &Card) -> Option<String> {
        let value = match &self.columns_by {
            ColumnsBy::State => Some(card.state.clone()),
            ColumnsBy::Property(p) => p.value(card).and_then(Value::as_str).map(str::to_owned),
        };
        value.or_else(|| self.columns.first().map(|c| c.value.clone()))
    }

    /// The board as `list_boards` lists it.
    pub fn summary(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "project": self.project })
    }

    /// The board's definition as `board` returns it.
    pub fn to_json(&self) -> Value {
        let by = |by: &Option<By>| by.as_ref().map(|b| json!({ "by": b.name() }));
        json!({
            "id": self.id,
            "name": self.name,
            "project": self.project,
            "columns": self.columns
                .iter()
                .map(|c| json!({ "value": c.value, "title": c.title }))
                .collect::<Vec<_>>(),
            "lanes": by(&self.lanes),
            "group": by(&self.group),
        })
    }

    /// The board's cards as lanes × columns × groups. Without lanes there is one lane, and without
    /// a group one group per cell, each with a null value and an empty title. A multi-select puts
    /// a card under each of its values; cards without a value come last, under "None". A card
    /// whose column value is no column's is `unplaced`.
    pub fn view(&self, cards: &[Card]) -> Value {
        let mut placed: Vec<(&Card, String)> = Vec::new();
        let mut unplaced = Vec::new();
        for card in cards.iter().filter(|card| self.includes(card)) {
            match self.column_of(card) {
                Some(column) if self.columns.iter().any(|c| c.value == column) => {
                    placed.push((card, column));
                }
                _ => unplaced.push(card_json(card)),
            }
        }
        let on_board: Vec<&Card> = placed.iter().map(|(card, _)| *card).collect();
        let single = || {
            vec![Bucket {
                value: None,
                title: String::new(),
            }]
        };
        let lanes = match &self.lanes {
            Some(by) => by.buckets(&on_board, true),
            None => single(),
        };
        let in_bucket = |by: &Option<By>, bucket: &Bucket, card: &Card| {
            by.as_ref()
                .is_none_or(|by| by.values(card).contains(&bucket.value))
        };
        let lanes: Vec<Value> = lanes
            .iter()
            .map(|lane| {
                let cells: Vec<Value> = self
                    .columns
                    .iter()
                    .map(|column| {
                        let cell: Vec<&Card> = placed
                            .iter()
                            .filter(|(card, value)| {
                                *value == column.value && in_bucket(&self.lanes, lane, card)
                            })
                            .map(|(card, _)| *card)
                            .collect();
                        let groups = match &self.group {
                            Some(by) => by.buckets(&cell, false),
                            None => single(),
                        };
                        let groups: Vec<Value> = groups
							.iter()
							.map(|group| {
								let cards: Vec<Value> = cell
									.iter()
									.filter(|card| in_bucket(&self.group, group, card))
									.map(|card| card_json(card))
									.collect();
								json!({ "value": group.value, "title": group.title, "cards": cards })
							})
							.collect();
                        json!({ "column": column.value, "groups": groups })
                    })
                    .collect();
                json!({ "value": lane.value, "title": lane.title, "cells": cells })
            })
            .collect();
        json!({ "board": self.to_json(), "lanes": lanes, "unplaced": unplaced })
    }
}

/// The arguments of Issues' `update` that move a card to the column `value`.
pub fn update_args(board: &Board, number: u32, value: &str) -> Value {
    match &board.columns_by {
        ColumnsBy::State => json!({ "number": number, "state": value }),
        ColumnsBy::Property(p) => {
            json!({ "number": number, "properties": { p.name.as_str(): value } })
        }
    }
}

/// The card after a move to the column `value`.
pub fn moved(board: &Board, card: &Card, value: &str) -> Card {
    let mut card = card.clone();
    match &board.columns_by {
        ColumnsBy::State => card.state = value.to_owned(),
        ColumnsBy::Property(p) => {
            card.properties.insert(p.name.clone(), json!(value));
        }
    }
    card
}

/// The boards in Kanban's settings (`boards`), in id order, and the definitions that are not
/// boards with their problem.
pub fn boards_from_settings(settings: &Value) -> (Vec<Board>, Vec<(String, String)>) {
    let mut boards = Vec::new();
    let mut invalid = Vec::new();
    if let Some(defined) = settings["boards"].as_object() {
        for (id, definition) in defined {
            if !valid_id(id) {
                invalid.push((
                    id.clone(),
                    "a board id is 1 to 64 lowercase letters, digits and -".to_owned(),
                ));
                continue;
            }
            match Board::from_definition(id, definition) {
                Ok(board) => boards.push(board),
                Err(error) => invalid.push((id.clone(), error_text(&error))),
            }
        }
    }
    boards.sort_by(|a, b| a.id.cmp(&b.id));
    (boards, invalid)
}

/// Whether `id` can name a board: 1 to 64 lowercase letters, digits and `-`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn error_text(error: &Error) -> String {
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
        "number": card.number,
        "title": card.title,
        "state": card.state,
        "project": card.project,
        "properties": card.properties,
        "version": card.version,
    })
}

/// The columns: "state", a select property (every option in order), or `{property, values}`
/// (some options in a chosen order). Titles come from the property's labels.
fn columns_from(columns: &Value) -> Result<(ColumnsBy, Vec<Column>), Error> {
    if columns.as_str() == Some("state") {
        let state = |value: &str, title: &str| Column {
            value: value.to_owned(),
            title: title.to_owned(),
        };
        return Ok((
            ColumnsBy::State,
            vec![state("open", "Open"), state("closed", "Closed")],
        ));
    }
    let (property, values) = match columns.get("property") {
        Some(property) => {
            let property = Property::from_value(property)?;
            let values = columns["values"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            (property, values)
        }
        None if columns.is_object() => {
            let property = Property::from_value(columns)?;
            let values = property.options.clone();
            (property, values)
        }
        None => {
            return Err(Error::invalid(
                "columns is a select property, {property, values} or \"state\"",
            ));
        }
    };
    if property.kind != "select" {
        return Err(Error::invalid(format!(
            "columns is a select property, not a {}",
            property.kind
        )));
    }
    if values.is_empty() {
        return Err(Error::invalid("a board has at least one column"));
    }
    let mut out: Vec<Column> = Vec::with_capacity(values.len());
    for value in values {
        if !property.options.contains(&value) {
            return Err(Error::invalid(format!(
                "{value} is not an option of {}",
                property.name
            )));
        }
        if out.iter().any(|c| c.value == value) {
            return Err(Error::invalid(format!("column {value} is repeated")));
        }
        out.push(Column {
            title: property.title(&value),
            value,
        });
    }
    Ok((ColumnsBy::Property(property), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Value {
        json!({
            "kind": "select", "name": "status", "options": ["todo", "doing", "done"],
            "labels": {"todo": "To do"},
        })
    }

    fn priority() -> Value {
        json!({ "kind": "select", "name": "priority", "options": ["p1", "p2", "p3"] })
    }

    fn tags() -> Value {
        json!({ "kind": "multiselect", "name": "tags", "options": ["bug", "docs"], "labels": {"bug": "Bug"} })
    }

    fn card(number: u32, project: Option<&str>, properties: Value) -> Card {
        Card {
            repo_id: "repo-1".to_owned(),
            number,
            title: format!("#{number}"),
            state: "open".to_owned(),
            project: project.map(str::to_owned),
            properties: properties.as_object().cloned().unwrap_or_default(),
            version: 1,
        }
    }

    fn board(definition: Value) -> Board {
        Board::from_definition("b", &definition).expect("a board")
    }

    fn numbers(cards: &Value) -> Vec<u64> {
        cards
            .as_array()
            .expect("cards")
            .iter()
            .filter_map(|c| c["number"].as_u64())
            .collect()
    }

    #[test]
    fn boards_come_from_the_settings() {
        let settings = json!({"boards": {
            "triage": {"name": "Triage", "columns": status(), "lanes": priority(), "group": "project"},
            "lsp": {"name": "LSP", "project": "crates/lsp", "columns": "state"},
            "bad": {"name": "Bad", "columns": {"kind": "shorttext", "name": "summary"}},
            "Upper": {"name": "Upper", "columns": "state"},
        }});
        let (boards, invalid) = boards_from_settings(&settings);
        let ids: Vec<&str> = boards.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, vec!["lsp", "triage"]);
        let bad: Vec<&str> = invalid.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(bad, vec!["Upper", "bad"]);
        assert_eq!(
            boards[0].summary(),
            json!({"id": "lsp", "name": "LSP", "project": "crates/lsp"})
        );
        assert_eq!(
            boards[1].to_json(),
            json!({
                "id": "triage", "name": "Triage", "project": null,
                "columns": [
                    {"value": "todo", "title": "To do"},
                    {"value": "doing", "title": "doing"},
                    {"value": "done", "title": "done"},
                ],
                "lanes": {"by": "priority"},
                "group": {"by": "project"},
            })
        );
    }

    #[test]
    fn columns_are_a_property_a_subset_or_state() {
        let subset = board(json!({"columns": {"property": status(), "values": ["done", "todo"]}}));
        let values: Vec<&str> = subset.columns.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["done", "todo"]);
        assert_eq!(subset.columns[1].title, "To do");
        let state = board(json!({"columns": "state"}));
        assert_eq!(
            state.columns[0],
            Column {
                value: "open".into(),
                title: "Open".into()
            }
        );
        for bad in [
            json!({}),
            json!({"columns": "status"}),
            json!({"columns": tags()}),
            json!({"columns": {"property": status(), "values": []}}),
            json!({"columns": {"property": status(), "values": ["blocked"]}}),
            json!({"columns": {"property": status(), "values": ["todo", "todo"]}}),
            json!({"columns": status(), "lanes": {"kind": "number", "name": "size"}}),
            json!({"columns": status(), "group": "state"}),
            json!({"columns": status(), "project": ""}),
        ] {
            assert!(Board::from_definition("b", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn without_lanes_or_groups_there_is_one_of_each() {
        let view = board(json!({"columns": status()})).view(&[
            card(1, None, json!({})),
            card(2, None, json!({"status": "doing"})),
            card(3, None, json!({"status": "blocked"})),
        ]);
        let lanes = view["lanes"].as_array().expect("lanes");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0]["value"], Value::Null);
        assert_eq!(lanes[0]["title"], "");
        let cells = lanes[0]["cells"].as_array().expect("cells");
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0]["column"], "todo");
        for cell in cells {
            assert_eq!(cell["groups"].as_array().map(Vec::len), Some(1));
            assert_eq!(cell["groups"][0]["value"], Value::Null);
        }
        // No value: the first column. A value that is no column's: unplaced.
        assert_eq!(numbers(&cells[0]["groups"][0]["cards"]), vec![1]);
        assert_eq!(numbers(&cells[1]["groups"][0]["cards"]), vec![2]);
        assert_eq!(numbers(&view["unplaced"]), vec![3]);
        assert_eq!(
            view["unplaced"][0],
            json!({
                "number": 3, "title": "#3", "state": "open", "project": null,
                "properties": {"status": "blocked"}, "version": 1,
            })
        );
    }

    #[test]
    fn a_default_places_a_card_without_a_value() {
        let mut defaulted = status();
        defaulted["default"] = json!("doing");
        let view = board(json!({"columns": defaulted})).view(&[card(1, None, json!({}))]);
        assert_eq!(
            numbers(&view["lanes"][0]["cells"][1]["groups"][0]["cards"]),
            vec![1]
        );
    }

    #[test]
    fn lanes_by_priority_end_with_none() {
        let view = board(json!({"columns": status(), "lanes": priority()})).view(&[
            card(1, None, json!({"priority": "p1"})),
            card(2, None, json!({})),
        ]);
        let lanes = view["lanes"].as_array().expect("lanes");
        let titles: Vec<&str> = lanes.iter().filter_map(|l| l["title"].as_str()).collect();
        assert_eq!(titles, vec!["p1", "p2", "p3", "None"]);
        assert_eq!(lanes[3]["value"], Value::Null);
        assert_eq!(
            numbers(&lanes[0]["cells"][0]["groups"][0]["cards"]),
            vec![1]
        );
        assert_eq!(
            numbers(&lanes[1]["cells"][0]["groups"][0]["cards"]),
            Vec::<u64>::new()
        );
        assert_eq!(
            numbers(&lanes[3]["cells"][0]["groups"][0]["cards"]),
            vec![2]
        );
    }

    #[test]
    fn a_multi_select_puts_a_card_under_each_value() {
        let view = board(json!({"columns": status(), "lanes": tags()})).view(&[
            card(1, None, json!({"tags": ["docs", "bug"]})),
            card(2, None, json!({"tags": []})),
        ]);
        let lanes = view["lanes"].as_array().expect("lanes");
        let titles: Vec<&str> = lanes.iter().filter_map(|l| l["title"].as_str()).collect();
        assert_eq!(titles, vec!["Bug", "docs", "None"]);
        assert_eq!(
            numbers(&lanes[0]["cells"][0]["groups"][0]["cards"]),
            vec![1]
        );
        assert_eq!(
            numbers(&lanes[1]["cells"][0]["groups"][0]["cards"]),
            vec![1]
        );
        assert_eq!(
            numbers(&lanes[2]["cells"][0]["groups"][0]["cards"]),
            vec![2]
        );
    }

    #[test]
    fn groups_by_project_hold_only_their_cards() {
        let view = board(json!({"columns": status(), "group": "project"})).view(&[
            card(1, Some("crates/lsp"), json!({})),
            card(2, None, json!({})),
            card(3, Some("crates/api"), json!({})),
            card(4, None, json!({"status": "done"})),
        ]);
        let cells = &view["lanes"][0]["cells"];
        let groups = cells[0]["groups"].as_array().expect("groups");
        let titles: Vec<&str> = groups.iter().filter_map(|g| g["title"].as_str()).collect();
        assert_eq!(titles, vec!["crates/api", "crates/lsp", "None"]);
        assert_eq!(numbers(&groups[2]["cards"]), vec![2]);
        // An empty cell has no groups; the third column holds only the None group.
        assert_eq!(cells[1]["groups"], json!([]));
        assert_eq!(cells[2]["groups"][0]["title"], "None");
    }

    #[test]
    fn groups_by_person_are_sorted_ids() {
        let owner = json!({"kind": "person", "name": "owner"});
        let view = board(json!({"columns": "state", "lanes": "project", "group": owner})).view(&[
            card(1, None, json!({"owner": "usr_zoe"})),
            card(2, None, json!({"owner": "usr_ada"})),
        ]);
        let lanes = view["lanes"].as_array().expect("lanes");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0]["title"], "None");
        let groups = &lanes[0]["cells"][0]["groups"];
        assert_eq!(groups[0]["value"], "usr_ada");
        assert_eq!(groups[1]["value"], "usr_zoe");
    }

    #[test]
    fn a_project_board_shows_only_its_project() {
        let lsp = board(json!({"project": "crates/lsp", "columns": "state"}));
        assert!(lsp.includes(&card(1, Some("crates/lsp"), json!({}))));
        assert!(!lsp.includes(&card(2, None, json!({}))));
        assert!(!lsp.includes(&card(3, Some("crates/lsp/sub"), json!({}))));
    }

    #[test]
    fn moves_update_the_column_property_or_the_state() {
        let by_status = board(json!({"columns": status()}));
        assert_eq!(
            update_args(&by_status, 3, "done"),
            json!({"number": 3, "properties": {"status": "done"}})
        );
        let moved_card = moved(&by_status, &card(3, None, json!({})), "done");
        assert_eq!(moved_card.properties["status"], "done");
        let by_state = board(json!({"columns": "state"}));
        assert_eq!(
            update_args(&by_state, 3, "closed"),
            json!({"number": 3, "state": "closed"})
        );
        assert_eq!(
            moved(&by_state, &card(3, None, json!({})), "closed").state,
            "closed"
        );
    }
}
