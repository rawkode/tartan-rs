//! Boards and cards, independent of storage.
//!
//! A board comes from Kanban's settings as `cue export` gives them. Kanban's CUE
//! module has already checked each board against the Issues properties the
//! repository binds as `#issues`, so a property reference arrives as the
//! property itself (its `name`, `kind`, `options`, `labels` and `default`).
//! CUE compares values, not references, so it cannot tell that `#issues` is the
//! repository's Issues entry: [`issues_mismatch`] checks each board against the
//! properties the installed Issues reports. The other checks here keep a
//! hand-written configuration from producing a broken view.

use tartan_ext::Error;
use tartan_ext::did::is_did;
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

    /// Whether `other`, a property as Issues' `properties` tool lists it, is this one: the same
    /// name, kind, options, labels and default.
    fn same_as(&self, other: &Value) -> bool {
        Self::from_value(other).is_ok_and(|other| other == *self)
    }

    /// The card's value of this property, or the property's default when the issue never set it,
    /// as Issues shows it.
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
                person: match self {
                    Self::Property(p) if p.kind == "person" && is_did(&value) => {
                        Some(value.clone())
                    }
                    _ => None,
                },
                value: Some(value),
            })
            .collect();
        if present.contains(&None) {
            buckets.push(Bucket {
                value: None,
                title: NONE_TITLE.to_owned(),
                person: None,
            });
        }
        buckets
    }
}

/// A lane or a group: a value and its title; None holds cards without a value. A person
/// property's lane or group is keyed by the DID, and `person` names it so the UI labels it by
/// handle; a value stored before persons were DIDs has no `person` and shows as its text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bucket {
    value: Option<String>,
    title: String,
    person: Option<String>,
}

impl Bucket {
    /// The lane or group as the `board` tool answers it.
    fn to_json(&self, extra: (&str, Value)) -> Value {
        let mut value = json!({ "value": self.value, "title": self.title });
        if let Some(did) = &self.person {
            value["person"] = json!(did);
        }
        value[extra.0] = extra.1;
        value
    }
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

    /// Whether the board uses an Issues property (rather than only state and project).
    pub fn uses_properties(&self) -> bool {
        !self.properties().is_empty()
    }

    /// The properties the board uses, with what each is for.
    fn properties(&self) -> Vec<(&'static str, &Property)> {
        let mut used = Vec::new();
        if let ColumnsBy::Property(p) = &self.columns_by {
            used.push(("columns", p));
        }
        for (what, by) in [("lanes", &self.lanes), ("group", &self.group)] {
            if let Some(By::Property(p)) = by {
                used.push((what, p));
            }
        }
        used
    }

    /// The column value of `card`: its value, else the property's default (Issues shows an
    /// issue that never set the property with the default), else the first column. A value or
    /// default that is no column's leaves the card unplaced.
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
                person: None,
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
                                group.to_json(("cards", json!(cards)))
                            })
                            .collect();
                        json!({ "column": column.value, "groups": groups })
                    })
                    .collect();
                lane.to_json(("cells", json!(cells)))
            })
            .collect();
        json!({ "board": self.to_json(), "lanes": lanes, "unplaced": unplaced })
    }
}

/// Why `board` does not fit the repository's Issues properties (`properties`, as Issues'
/// `properties` tool lists them), if it does not: each property it uses must be one of them,
/// with the same definition.
pub fn issues_mismatch(board: &Board, properties: &[Value]) -> Option<String> {
    board.properties().into_iter().find_map(|(what, property)| {
        match properties
            .iter()
            .find(|p| p["name"] == property.name.as_str())
        {
            None => Some(format!(
                "{what}: Issues on this repository has no property {}",
                property.name
            )),
            Some(issues) if !property.same_as(issues) => Some(format!(
                "{what}: {} is not the repository's Issues property {}: its definition differs",
                property.name, property.name
            )),
            Some(_) => None,
        }
    })
}

/// A card from an issue as Issues gives it (a snapshot event's, or a tool's result), on the
/// repository `repo_id`.
pub fn card_from_issue(repo_id: &str, issue: &Value) -> Option<Card> {
    let number = issue["number"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())?;
    let version = issue["version"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())?;
    Some(Card {
        repo_id: repo_id.to_owned(),
        number,
        title: issue["title"].as_str().unwrap_or_default().to_owned(),
        state: issue["state"].as_str().unwrap_or("open").to_owned(),
        project: issue["project"].as_str().map(str::to_owned),
        properties: issue["properties"].as_object().cloned().unwrap_or_default(),
        version,
    })
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
        let view = board(json!({"columns": defaulted.clone()})).view(&[card(1, None, json!({}))]);
        assert_eq!(
            numbers(&view["lanes"][0]["cells"][1]["groups"][0]["cards"]),
            vec![1]
        );
        // The default is the issue's value, as Issues shows it: on a board without its column
        // the card is unplaced, as one with that value set would be.
        let subset = board(json!({"columns": {"property": defaulted, "values": ["todo", "done"]}}));
        let view = subset.view(&[
            card(1, None, json!({})),
            card(2, None, json!({"status": "doing"})),
        ]);
        assert_eq!(numbers(&view["unplaced"]), vec![1, 2]);
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
    fn groups_by_person_are_sorted_dids() {
        let owner = json!({"kind": "person", "name": "owner"});
        let view = board(json!({"columns": "state", "lanes": "project", "group": owner})).view(&[
            card(1, None, json!({"owner": BOB})),
            card(2, None, json!({"owner": ALICE})),
        ]);
        let lanes = view["lanes"].as_array().expect("lanes");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0]["title"], "None");
        assert!(lanes[0].get("person").is_none());
        let groups = &lanes[0]["cells"][0]["groups"];
        assert_eq!(groups[0]["value"], ALICE);
        assert_eq!(groups[0]["person"], ALICE);
        assert_eq!(groups[1]["value"], BOB);
    }

    #[test]
    fn lanes_by_person_are_keyed_by_did_and_legacy_values_are_text() {
        let assignee = json!({"kind": "person", "name": "assignee"});
        let view = board(json!({"columns": "state", "lanes": assignee})).view(&[
            card(1, None, json!({"assignee": ALICE})),
            card(2, None, json!({"assignee": "admin"})),
            card(3, None, json!({})),
        ]);
        let lanes = view["lanes"].as_array().expect("lanes");
        let keys: Vec<&Value> = lanes.iter().map(|l| &l["value"]).collect();
        assert_eq!(keys, vec![&json!("admin"), &json!(ALICE), &Value::Null]);
        assert!(lanes[0].get("person").is_none());
        assert_eq!(lanes[1]["person"], ALICE);
        assert_eq!(lanes[1]["cells"][0]["groups"][0]["cards"][0]["number"], 1);
        assert_eq!(lanes[2]["title"], "None");
    }

    const ALICE: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb";

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
        let by_state = board(json!({"columns": "state"}));
        assert_eq!(
            update_args(&by_state, 3, "closed"),
            json!({"number": 3, "state": "closed"})
        );
    }

    #[test]
    fn cards_come_from_issues() {
        let issue = json!({
            "number": 7, "title": "Retitled", "state": "closed", "project": "crates/lsp",
            "properties": {"status": "done"}, "version": 3, "author": {"kind": "user"},
        });
        let card = card_from_issue("repo-1", &issue).expect("a card");
        assert_eq!(
            (
                card.number,
                card.title.as_str(),
                card.state.as_str(),
                card.version
            ),
            (7, "Retitled", "closed", 3)
        );
        assert_eq!(card.project.as_deref(), Some("crates/lsp"));
        assert_eq!(card.properties["status"], "done");
        assert!(card_from_issue("repo-1", &json!({"number": 7})).is_none());
    }

    #[test]
    fn boards_must_use_the_installed_issues_properties() {
        let triage = board(json!({"columns": status(), "lanes": priority(), "group": "project"}));
        let installed = [status(), priority(), tags()];
        assert_eq!(issues_mismatch(&triage, &installed), None);
        // Bound to another Issues entry: a property the installation does not have.
        let phase = json!({"kind": "select", "name": "phase", "options": ["a", "b"]});
        let fake = board(json!({"columns": phase}));
        assert_eq!(
            issues_mismatch(&fake, &installed).as_deref(),
            Some("columns: Issues on this repository has no property phase")
        );
        // The same name with another definition.
        let mut other = priority();
        other["options"] = json!(["p1", "p2"]);
        let lanes = issues_mismatch(&triage, &[status(), other]).expect("a mismatch");
        assert!(lanes.starts_with("lanes: priority is not"), "{lanes}");
        assert_eq!(
            issues_mismatch(&board(json!({"columns": "state"})), &[]),
            None
        );
    }
}
