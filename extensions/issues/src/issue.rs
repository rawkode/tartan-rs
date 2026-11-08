//! Issues without the host: argument checks, typed property values, JSON
//! shapes and the UI documents. Pure, so `cargo test` covers it on the host
//! target.

use serde_json::{json, Map, Value};
use tartan_ext::serde_json;
use tartan_ext::ui::{self, Tone};
use tartan_ext::{Actor, ActorKind, Error};

/// A title's limit, in characters (the tool schema's `maxLength`).
pub const TITLE_MAX_CHARS: usize = 200;
/// A body's limit, in characters.
pub const BODY_MAX_CHARS: usize = 20_000;
pub const LIST_DEFAULT_LIMIT: u32 = 30;
pub const LIST_MAX_LIMIT: u32 = 100;
/// The Issues tab shows at most this many issues.
pub const TAB_LIMIT: u32 = 100;
/// The largest issue number a tool accepts.
pub const NUMBER_MAX: u32 = i32::MAX as u32;
/// A project's root path, in bytes: the walk's 12 levels of the longest names.
pub const PROJECT_MAX_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
	Open,
	Closed,
}

impl State {
	pub fn as_str(self) -> &'static str {
		match self {
			State::Open => "open",
			State::Closed => "closed",
		}
	}

	pub fn parse(s: &str) -> Option<State> {
		match s {
			"open" => Some(State::Open),
			"closed" => Some(State::Closed),
			_ => None,
		}
	}
}

/// The `list` tool's `state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
	Only(State),
	All,
}

impl Filter {
	pub fn as_str(self) -> &'static str {
		match self {
			Filter::Only(s) => s.as_str(),
			Filter::All => "all",
		}
	}
}

/// Who opened an issue: the actor the host gave the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Author {
	pub kind: String,
	pub id: String,
	pub on_behalf_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
	pub number: u32,
	pub title: String,
	pub body: String,
	/// The project's root path; None for an issue of the whole repository.
	pub project: Option<String>,
	pub state: State,
	pub author: Author,
	pub created_at: u64,
	pub updated_at: u64,
	pub closed_at: Option<u64>,
	/// Stored property values: a property's default is filled in when it is read.
	pub properties: Map<String, Value>,
	/// Goes up with every change, so a write can name what it read.
	pub version: u32,
}

/// Properties shown as table columns: the UI allows 12, and 6 are the issue's own.
pub const PROPERTY_COLUMNS_MAX: usize = 6;
/// Options a select offers: the UI allows 99, and one is "not set".
pub const PROPERTY_OPTIONS_SHOWN: usize = 98;
/// Properties the form offers: the UI allows 30, and two are the title and body.
pub const FORM_PROPERTIES_MAX: usize = 28;
/// A short text's limit, in characters (`types.#ShortTextMax`).
pub const SHORT_TEXT_MAX_CHARS: usize = 200;
/// A long text's limit, in characters (`types.#LongTextMax`).
pub const LONG_TEXT_MAX_CHARS: usize = 20_000;
/// The largest amount of money, in minor units: what a JSON number keeps exact.
pub const MONEY_MAX: i64 = 9_007_199_254_740_991;
/// A principal id's limit, in characters (`types.#PersonValue`).
pub const PERSON_MAX_CHARS: usize = 128;
/// A URL's limit, in characters (`types.#URLValue`).
pub const URL_MAX_CHARS: usize = 2048;
/// A table cell shows at most this many characters of a text.
const CELL_MAX_CHARS: usize = 80;

/// The kinds of property in `tartan.dev/types`. Issues stores and shows each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	ShortText,
	LongText,
	Number,
	Date,
	Boolean,
	Select,
	MultiSelect,
	Person,
	Url,
	Money,
}

impl Kind {
	pub fn parse(kind: &str) -> Option<Kind> {
		Some(match kind {
			"shorttext" => Kind::ShortText,
			"longtext" => Kind::LongText,
			"number" => Kind::Number,
			"date" => Kind::Date,
			"boolean" => Kind::Boolean,
			"select" => Kind::Select,
			"multiselect" => Kind::MultiSelect,
			"person" => Kind::Person,
			"url" => Kind::Url,
			"money" => Kind::Money,
			_ => return None,
		})
	}

	pub fn as_str(self) -> &'static str {
		match self {
			Kind::ShortText => "shorttext",
			Kind::LongText => "longtext",
			Kind::Number => "number",
			Kind::Date => "date",
			Kind::Boolean => "boolean",
			Kind::Select => "select",
			Kind::MultiSelect => "multiselect",
			Kind::Person => "person",
			Kind::Url => "url",
			Kind::Money => "money",
		}
	}
}

/// A property from the repository's configuration (`settings.properties`),
/// typed with `tartan.dev/types`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyDef {
	pub name: String,
	/// None for a kind this version does not know: it holds nothing.
	pub kind: Option<Kind>,
	/// The kind as configured, so an unknown one can still be shown.
	pub kind_name: String,
	pub label: Option<String>,
	pub options: Vec<String>,
	/// Titles of options, by option.
	pub labels: Map<String, Value>,
	pub default: Option<Value>,
	pub min: Option<Value>,
	pub max: Option<Value>,
	pub max_length: Option<usize>,
	pub currency: Option<String>,
}

impl PropertyDef {
	/// The title people see: the label, or the name.
	pub fn title(&self) -> &str {
		self.label.as_deref().unwrap_or(&self.name)
	}

	/// The title of an option: its label, or the option itself.
	pub fn option_title<'a>(&'a self, option: &'a str) -> &'a str {
		self.labels.get(option).and_then(Value::as_str).unwrap_or(option)
	}
}

/// The properties of the installation's settings, in name order. CUE has
/// checked their shape against Issues' module.
pub fn property_defs(settings: &Value) -> Vec<PropertyDef> {
	let mut defs: Vec<PropertyDef> = settings["properties"]
		.as_object()
		.map(|properties| {
			properties
				.iter()
				.map(|(name, def)| {
					let kind_name = def["kind"].as_str().unwrap_or_default().to_string();
					PropertyDef {
						name: name.clone(),
						kind: Kind::parse(&kind_name),
						kind_name,
						label: def["label"].as_str().map(str::to_string),
						options: def["options"]
							.as_array()
							.map(|o| o.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
							.unwrap_or_default(),
						labels: def["labels"].as_object().cloned().unwrap_or_default(),
						default: def.get("default").filter(|v| !v.is_null()).cloned(),
						min: def.get("min").filter(|v| v.is_number()).cloned(),
						max: def.get("max").filter(|v| v.is_number()).cloned(),
						max_length: def["maxLength"].as_u64().and_then(|n| usize::try_from(n).ok()),
						currency: def["currency"].as_str().map(str::to_string),
					}
				})
				.collect()
		})
		.unwrap_or_default();
	defs.sort_by(|a, b| a.name.cmp(&b.name));
	defs
}

/// The `properties` tool's list: each property as configured.
pub fn defs_json(defs: &[PropertyDef]) -> Value {
	json!(defs
		.iter()
		.map(|d| {
			let mut v = json!({ "name": d.name, "kind": d.kind_name });
			if let Some(label) = &d.label {
				v["label"] = json!(label);
			}
			if !d.options.is_empty() {
				v["options"] = json!(d.options);
			}
			if !d.labels.is_empty() {
				v["labels"] = Value::Object(d.labels.clone());
			}
			if let Some(default) = &d.default {
				v["default"] = default.clone();
			}
			if let Some(min) = &d.min {
				v["min"] = min.clone();
			}
			if let Some(max) = &d.max {
				v["max"] = max.clone();
			}
			if let Some(max_length) = d.max_length {
				v["maxLength"] = json!(max_length);
			}
			if let Some(currency) = &d.currency {
				v["currency"] = json!(currency);
			}
			v
		})
		.collect::<Vec<_>>())
}

/// Whether `YYYY-MM-DD` is a day on the calendar.
fn is_date(text: &str) -> bool {
	let b = text.as_bytes();
	let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
	if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !digits(0..4) || !digits(5..7) || !digits(8..10) {
		return false;
	}
	let number = |r: std::ops::Range<usize>| text[r].parse::<u32>().unwrap_or(0);
	let (year, month, day) = (number(0..4), number(5..7), number(8..10));
	let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
	let days = match month {
		1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
		4 | 6 | 9 | 11 => 30,
		2 if leap => 29,
		2 => 28,
		_ => return false,
	};
	(1..=days).contains(&day)
}

/// Whether `text` is a principal id: 1 to 128 letters, digits and `._:@-`,
/// starting with a letter or digit (`types.#PersonValue`).
fn is_person(text: &str) -> bool {
	let mut chars = text.chars();
	chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
		&& text.chars().count() <= PERSON_MAX_CHARS
		&& chars.all(|c| c.is_ascii_alphanumeric() || "._:@-".contains(c))
}

/// Whether `text` is an absolute `http` or `https` URL of at most 2048
/// characters, without whitespace, and with a host (`types.#URLValue`).
fn is_url(text: &str) -> bool {
	let rest = text
		.strip_prefix("https://")
		.or_else(|| text.strip_prefix("http://"));
	let Some(rest) = rest else {
		return false;
	};
	let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
	host_end > 0 && text.chars().count() <= URL_MAX_CHARS && !text.chars().any(char::is_whitespace)
}

/// Whether `text` fits a text property: at most `max_length` characters (and
/// never more than `limit`), on one line unless `multiline`.
fn fits_text(text: &str, max_length: Option<usize>, limit: usize, multiline: bool) -> bool {
	let limit = max_length.unwrap_or(limit).min(limit);
	text.chars().count() <= limit && (multiline || !text.contains(['\n', '\r']))
}

/// Whether a value suits a property of this configuration. A kind this
/// version does not know holds nothing, so a write to it is refused.
fn fits(def: &PropertyDef, value: &Value) -> bool {
	let bound = |limit: &Option<Value>, within: fn(f64, f64) -> bool, n: f64| {
		limit
			.as_ref()
			.and_then(Value::as_f64)
			.is_none_or(|l| within(n, l))
	};
	let option = |v: &Value| v.as_str().is_some_and(|s| def.options.iter().any(|o| o == s));
	let Some(kind) = def.kind else {
		return false;
	};
	match kind {
		Kind::ShortText => value
			.as_str()
			.is_some_and(|s| fits_text(s, def.max_length, SHORT_TEXT_MAX_CHARS, false)),
		Kind::LongText => value
			.as_str()
			.is_some_and(|s| fits_text(s, def.max_length, LONG_TEXT_MAX_CHARS, true)),
		Kind::Number => value
			.as_f64()
			.is_some_and(|n| bound(&def.min, |n, l| n >= l, n) && bound(&def.max, |n, l| n <= l, n)),
		Kind::Select => option(value),
		Kind::MultiSelect => value.as_array().is_some_and(|items| {
			items
				.iter()
				.enumerate()
				.all(|(i, item)| option(item) && !items[..i].contains(item))
		}),
		Kind::Money => value
			.as_i64()
			.is_some_and(|n| (-MONEY_MAX..=MONEY_MAX).contains(&n)),
		Kind::Date => value.as_str().is_some_and(is_date),
		Kind::Boolean => value.is_boolean(),
		Kind::Person => value.as_str().is_some_and(is_person),
		Kind::Url => value.as_str().is_some_and(is_url),
	}
}

/// The decimal places of a currency's minor unit (ISO 4217).
pub fn minor_digits(currency: &str) -> usize {
	match currency {
		"BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX" | "UYI"
		| "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
		"BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
		"CLF" | "UYW" => 4,
		_ => 2,
	}
}

/// `12.50` in GBP is 1250. Decimal text only, so no amount goes through a float.
pub fn parse_money(text: &str, currency: &str) -> Option<i64> {
	let places = minor_digits(currency);
	let text = text.trim();
	let (negative, digits) = match text.strip_prefix('-') {
		Some(rest) => (true, rest),
		None => (false, text),
	};
	let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
	let numeric = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
	if (whole.is_empty() && fraction.is_empty())
		|| !numeric(whole)
		|| !numeric(fraction)
		|| fraction.len() > places
	{
		return None;
	}
	let amount = format!("{whole}{fraction:0<places$}").parse::<i64>().ok()?;
	(amount <= MONEY_MAX).then_some(if negative { -amount } else { amount })
}

/// 1250 in GBP is `GBP 12.50`.
pub fn format_money(amount: i64, currency: &str) -> String {
	let places = minor_digits(currency);
	let sign = if amount < 0 { "-" } else { "" };
	let abs = amount.unsigned_abs();
	if places == 0 {
		return format!("{currency} {sign}{abs}");
	}
	let unit = 10u64.pow(places as u32);
	format!("{currency} {sign}{}.{:0places$}", abs / unit, abs % unit)
}

/// Checks written values against the current configuration. Null clears a
/// property. Only what is being written is checked: a stored value of a
/// property the configuration no longer has stays, and is never rewritten.
pub fn checked_properties(given: &Value, defs: &[PropertyDef]) -> Result<Map<String, Value>, Error> {
	let given = match given {
		Value::Null => return Ok(Map::new()),
		Value::Object(values) => values,
		_ => return Err(Error::invalid("properties is an object")),
	};
	for (name, value) in given {
		let def = defs.iter().find(|d| &d.name == name).ok_or_else(|| {
			Error::invalid(format!("no property {name} in this repository's configuration"))
		})?;
		if value.is_null() {
			continue;
		}
		if !fits(def, value) {
			return Err(Error::invalid(format!(
				"{name}: not a valid {} value",
				def.kind_name
			)));
		}
	}
	Ok(given.clone())
}

/// Stored values with the configuration's defaults for what an issue never set.
pub fn shown_properties(stored: &Map<String, Value>, defs: &[PropertyDef]) -> Map<String, Value> {
	let mut shown = stored.clone();
	for def in defs {
		if let (false, Some(default)) = (shown.contains_key(&def.name), &def.default) {
			shown.insert(def.name.clone(), default.clone());
		}
	}
	shown
}

/// A checked `create` input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewIssue {
	pub title: String,
	pub body: String,
	pub properties: Map<String, Value>,
}

/// One write of `update`, `close` or `reopen`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
	pub title: Option<String>,
	pub body: Option<String>,
	pub properties: Map<String, Value>,
	pub state: Option<State>,
	pub expected_version: Option<u32>,
}

fn title_of(value: &Value) -> Result<String, Error> {
	let title = match value {
		Value::String(s) => s.trim(),
		Value::Null => "",
		_ => return Err(Error::invalid("title is a string")),
	};
	if title.is_empty() {
		return Err(Error::invalid("title is required"));
	}
	if title.chars().count() > TITLE_MAX_CHARS {
		return Err(Error::invalid(format!(
			"title is limited to {TITLE_MAX_CHARS} characters"
		)));
	}
	if title.contains(['\n', '\r']) {
		return Err(Error::invalid("title is one line"));
	}
	Ok(title.to_string())
}

fn body_of(value: &Value) -> Result<String, Error> {
	let body = match value {
		Value::String(s) => s.trim_end(),
		Value::Null => "",
		_ => return Err(Error::invalid("body is a string")),
	};
	if body.chars().count() > BODY_MAX_CHARS {
		return Err(Error::invalid(format!(
			"body is limited to {BODY_MAX_CHARS} characters"
		)));
	}
	Ok(body.to_string())
}

/// `create` arguments: a trimmed, non-empty one-line title, an optional body and
/// values of the repository's properties.
pub fn new_issue(args: &Value, defs: &[PropertyDef]) -> Result<NewIssue, Error> {
	Ok(NewIssue {
		title: title_of(&args["title"])?,
		body: body_of(&args["body"])?,
		properties: checked_properties(&args["properties"], defs)?,
	})
}

/// `update` arguments; what is absent stays as it is.
pub fn change_args(args: &Value, defs: &[PropertyDef]) -> Result<Change, Error> {
	let state = match &args["state"] {
		Value::Null => None,
		Value::String(s) => Some(State::parse(s).ok_or_else(|| Error::invalid("state is open or closed"))?),
		_ => return Err(Error::invalid("state is open or closed")),
	};
	let expected_version = match &args["expectedVersion"] {
		Value::Null => None,
		v => Some(
			v.as_u64()
				.and_then(|n| u32::try_from(n).ok())
				.ok_or_else(|| Error::invalid("expectedVersion is an integer"))?,
		),
	};
	Ok(Change {
		title: match &args["title"] {
			Value::Null => None,
			v => Some(title_of(v)?),
		},
		body: match &args["body"] {
			Value::Null => None,
			v => Some(body_of(v)?),
		},
		properties: checked_properties(&args["properties"], defs)?,
		state,
		expected_version,
	})
}

/// The value of one text input: a number or an amount is read from its text.
fn form_text(def: &PropertyDef, text: &str) -> Result<Value, Error> {
	match def.kind {
		Some(Kind::Number) => text
			.parse::<i64>()
			.map(Value::from)
			.ok()
			.or_else(|| {
				text.parse::<f64>()
					.ok()
					.and_then(serde_json::Number::from_f64)
					.map(Value::Number)
			})
			.ok_or_else(|| Error::invalid(format!("{}: not a number", def.name))),
		Some(Kind::Money) => {
			let currency = def.currency.as_deref().unwrap_or_default();
			parse_money(text, currency)
				.map(Value::from)
				.ok_or_else(|| Error::invalid(format!("{}: not an amount of {currency}", def.name)))
		}
		Some(Kind::ShortText | Kind::Person | Kind::Url) => Ok(json!(text.trim())),
		_ => Ok(json!(text)),
	}
}

/// The form's input for the title. Property names start with a letter, so
/// the form's own inputs start with `_` and never meet a property's.
pub const FORM_TITLE: &str = "_title";
/// The form's input for the body.
pub const FORM_BODY: &str = "_body";

/// The `create` arguments of the tab's form: `_title`, `_body` and one input
/// per property, named after it. An empty input is not set, so the
/// property's default shows. A checkbox or a multi-select is always a value,
/// so a default can be turned off or emptied.
pub fn form_args(payload: &Value, defs: &[PropertyDef]) -> Result<Value, Error> {
	let mut properties = Map::new();
	for def in defs {
		let given = &payload[def.name.as_str()];
		let value = match given {
			Value::Bool(_) if def.kind == Some(Kind::Boolean) => given.clone(),
			Value::Array(_) if def.kind == Some(Kind::MultiSelect) => given.clone(),
			Value::String(text) if !text.trim().is_empty() => form_text(def, text)?,
			Value::Number(n) => form_text(def, &n.to_string())?,
			_ => continue,
		};
		properties.insert(def.name.clone(), value);
	}
	Ok(json!({
		"title": payload[FORM_TITLE],
		"body": payload[FORM_BODY],
		"properties": properties,
	}))
}

/// The optional `project` argument of every tool: a project's root path, as
/// the kernel passes it. The extension cannot see the project graph, so it
/// checks the shape only, and any directory git can hold has to pass: a
/// backslash or a newline in a name is a project like any other. Only tools
/// read it; the tab's form never sets one.
pub fn project_arg(args: &Value) -> Result<Option<String>, Error> {
	let root = match &args["project"] {
		Value::Null => return Ok(None),
		Value::String(s) => s.as_str(),
		_ => return Err(Error::invalid("project is a string")),
	};
	let clean = !root.is_empty()
		&& root.len() <= PROJECT_MAX_BYTES
		&& !root.contains('\0')
		&& root.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
	if !clean {
		return Err(Error::invalid(
			"project is a repository-relative root path, like crates/lsp",
		));
	}
	Ok(Some(root.to_string()))
}

/// The `number` argument of `get`, `close` and `reopen`.
pub fn number_arg(args: &Value) -> Result<u32, Error> {
	args["number"]
		.as_u64()
		.and_then(|n| u32::try_from(n).ok())
		.filter(|n| (1..=NUMBER_MAX).contains(n))
		.ok_or_else(|| Error::invalid(format!("number is an integer from 1 to {NUMBER_MAX}")))
}

/// The `list` tool's `before`: only issues numbered below it.
pub fn before_arg(args: &Value) -> Result<Option<u32>, Error> {
	match &args["before"] {
		Value::Null => Ok(None),
		v => v
			.as_u64()
			.and_then(|n| u32::try_from(n).ok())
			.map(Some)
			.ok_or_else(|| Error::invalid("before is an issue number")),
	}
}

/// The `list` tool's `state` (default open) and `limit` (default 30, ≤ 100).
pub fn list_args(args: &Value) -> Result<(Filter, u32), Error> {
	let filter = match &args["state"] {
		Value::Null => Filter::Only(State::Open),
		Value::String(s) if s == "all" => Filter::All,
		Value::String(s) => {
			Filter::Only(State::parse(s).ok_or_else(|| Error::invalid("state is open, closed or all"))?)
		}
		_ => return Err(Error::invalid("state is open, closed or all")),
	};
	let limit = match &args["limit"] {
		Value::Null => LIST_DEFAULT_LIMIT,
		v => v
			.as_u64()
			.and_then(|n| u32::try_from(n).ok())
			.filter(|n| (1..=LIST_MAX_LIMIT).contains(n))
			.ok_or_else(|| Error::invalid(format!("limit is an integer from 1 to {LIST_MAX_LIMIT}")))?,
	};
	Ok((filter, limit))
}

pub fn actor_kind(kind: ActorKind) -> &'static str {
	match kind {
		ActorKind::User => "user",
		ActorKind::Agent => "agent",
		ActorKind::Ext => "ext",
		ActorKind::System => "system",
	}
}

/// The author of a new issue: the call's actor (a tool's caller, a slot's
/// viewer). An anonymous call cannot open one.
pub fn author_of(actor: Option<&Actor>) -> Result<Author, Error> {
	let actor = actor.ok_or_else(|| Error::denied("an issue needs a signed-in author"))?;
	Ok(Author {
		kind: actor_kind(actor.kind).to_string(),
		id: actor.id.clone(),
		on_behalf_of: actor.on_behalf_of.clone(),
	})
}

fn author_json(a: &Author) -> Value {
	let mut v = json!({ "kind": a.kind, "id": a.id });
	if let Some(user) = &a.on_behalf_of {
		v["onBehalfOf"] = json!(user);
	}
	v
}

/// An issue as a tool returns it; `list` leaves the body out.
pub fn issue_json(i: &Issue, with_body: bool, defs: &[PropertyDef]) -> Value {
	let mut v = json!({
		"number": i.number,
		"title": i.title,
		"project": i.project,
		"state": i.state.as_str(),
		"author": author_json(&i.author),
		"createdAt": i.created_at,
		"updatedAt": i.updated_at,
		"closedAt": i.closed_at,
		"properties": shown_properties(&i.properties, defs),
		"version": i.version,
	});
	if with_body {
		v["body"] = json!(i.body);
	}
	v
}

/// `YYYY-MM-DD` (UTC) of a Unix time in milliseconds.
pub fn date(ms: u64) -> String {
	// Howard Hinnant's civil_from_days.
	let z = (ms / 86_400_000) as i64 + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z - era * 146_097;
	let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let day = doy - (153 * mp + 2) / 5 + 1;
	let month = if mp < 10 { mp + 3 } else { mp - 9 };
	let year = yoe + era * 400 + i64::from(month <= 2);
	format!("{year:04}-{month:02}-{day:02}")
}

fn author_text(a: &Author) -> String {
	match &a.on_behalf_of {
		Some(user) => format!("{} (for {user})", a.id),
		None => a.id.clone(),
	}
}

/// The action id of the tab's form.
pub const CREATE_ACTION: &str = "create";

/// The form that opens an issue from the tab.
pub fn create_form(defs: &[PropertyDef]) -> ui::Node {
	let mut inputs = vec![
		ui::input(FORM_TITLE, Some("Title"), true),
		ui::textarea(FORM_BODY, Some("Description (Markdown)"), false),
	];
	for def in defs.iter().filter(|d| d.kind.is_some()).take(FORM_PROPERTIES_MAX) {
		let name = def.name.as_str();
		let label = def.title();
		let choices = || {
			def.options
				.iter()
				.take(PROPERTY_OPTIONS_SHOWN)
				.map(|o| json!({ "value": o, "label": def.option_title(o) }))
		};
		let typed = |input_type: &str| {
			let mut input = ui::input(name, Some(label), false);
			input["inputType"] = json!(input_type);
			input
		};
		inputs.push(match def.kind {
			Some(Kind::Select) => {
				let mut options = vec![json!({ "value": "", "label": "—" })];
				options.extend(choices());
				json!({
					"t": "select",
					"name": name,
					"label": label,
					"options": options,
					"value": def.default.as_ref().and_then(Value::as_str).unwrap_or(""),
				})
			}
			Some(Kind::MultiSelect) => json!({
				"t": "select",
				"multiple": true,
				"name": name,
				"label": label,
				"options": choices().collect::<Vec<_>>(),
				"value": def.default.clone().unwrap_or_else(|| json!([])),
			}),
			Some(Kind::Boolean) => json!({
				"t": "checkbox",
				"name": name,
				"label": label,
				"value": def.default.as_ref().and_then(Value::as_bool).unwrap_or(false),
			}),
			Some(Kind::LongText) => ui::textarea(name, Some(&format!("{label} (Markdown)")), false),
			Some(Kind::Number) => typed("number"),
			Some(Kind::Date) => typed("date"),
			Some(Kind::Url) => typed("url"),
			Some(Kind::Money) => {
				let currency = def.currency.as_deref().unwrap_or_default();
				ui::input(name, Some(&format!("{label} ({currency})")), false)
			}
			_ => ui::input(name, Some(label), false),
		});
	}
	ui::form(inputs, "Open issue", ui::action(CREATE_ACTION, None, None))
}

/// At most `max` characters of `text`, on one line, with an ellipsis when cut.
fn excerpt(text: &str, max: usize) -> String {
	let line = text.lines().next().unwrap_or_default();
	if line.chars().count() <= max && line.len() == text.trim_end().len() {
		return line.to_string();
	}
	let mut cut: String = line.chars().take(max).collect();
	cut.push('…');
	cut
}

/// A property's value as text: options by their titles, money in its currency.
pub fn property_text(def: &PropertyDef, value: Option<&Value>) -> String {
	match (def.kind, value) {
		(_, None | Some(Value::Null)) => "—".to_string(),
		(Some(Kind::Money), Some(Value::Number(n))) => match (&def.currency, n.as_i64()) {
			(Some(currency), Some(amount)) => format_money(amount, currency),
			_ => n.to_string(),
		},
		(Some(Kind::Select), Some(Value::String(s))) => def.option_title(s).to_string(),
		(_, Some(Value::Number(n))) => n.to_string(),
		(_, Some(Value::String(s))) => s.clone(),
		(_, Some(Value::Bool(b))) => (if *b { "Yes" } else { "No" }).to_string(),
		(_, Some(Value::Array(items))) if !items.is_empty() => items
			.iter()
			.filter_map(Value::as_str)
			.map(|s| def.option_title(s))
			.collect::<Vec<_>>()
			.join(", "),
		_ => "—".to_string(),
	}
}

/// A property's value as a node: a person as an id, an `https` URL as a link,
/// long text as Markdown when `full`, or else as an excerpt.
fn property_node(def: &PropertyDef, value: Option<&Value>, full: bool) -> ui::Node {
	let text = value.and_then(Value::as_str);
	match (def.kind, text) {
		(Some(Kind::Person), Some(id)) => ui::mono(id),
		(Some(Kind::Url), Some(url)) if url.starts_with("https://") => {
			ui::link(excerpt(url, CELL_MAX_CHARS), url)
		}
		(Some(Kind::LongText), Some(md)) if full => ui::markdown(md),
		(Some(Kind::LongText), Some(md)) => json!(excerpt(md, CELL_MAX_CHARS)),
		_ => json!(property_text(def, value)),
	}
}

fn state_badge(state: State) -> ui::Node {
	match state {
		State::Open => ui::badge("open", Tone::Success),
		State::Closed => ui::badge("closed", Tone::Muted),
	}
}

/// `repo.tab` Issues: counts, the newest issues and the new-issue form. Long
/// texts are left out of the table: the issue view shows them.
pub fn tab_doc(issues: &[Issue], open: i64, closed: i64, defs: &[PropertyDef]) -> Value {
	let stats = ui::row(vec![
		ui::stat("Open", open as f64),
		ui::stat("Closed", closed as f64),
	]);
	let list = if issues.is_empty() {
		ui::empty(
			"No issues yet",
			Some("Open the first one below, or with the issues_create tool over MCP."),
		)
	} else {
		let shown_defs: Vec<&PropertyDef> = defs
			.iter()
			.filter(|d| d.kind.is_some() && d.kind != Some(Kind::LongText))
			.take(PROPERTY_COLUMNS_MAX)
			.collect();
		let mut columns = vec!["#", "Title"];
		columns.extend(shown_defs.iter().map(|d| d.title()));
		columns.extend(["Project", "State", "Author", "Updated"]);
		ui::table(
			&columns,
			issues
				.iter()
				.map(|i| {
					let shown = shown_properties(&i.properties, defs);
					let mut row = vec![json!(format!("#{}", i.number)), json!(i.title)];
					row.extend(
						shown_defs
							.iter()
							.map(|d| property_node(d, shown.get(&d.name), false)),
					);
					row.extend([
						i.project.as_deref().map_or(json!("—"), ui::mono),
						state_badge(i.state),
						ui::mono(author_text(&i.author)),
						json!(date(i.updated_at)),
					]);
					row
				})
				.collect(),
		)
	};
	ui::doc(ui::stack(vec![
		ui::heading("Issues", 2),
		stats,
		list,
		ui::section("New issue", vec![create_form(defs)]),
	]))
}

/// The `issue` slot: one issue with its body and every property; the body and
/// long texts render as Markdown.
pub fn issue_doc(i: &Issue, defs: &[PropertyDef]) -> Value {
	let shown = shown_properties(&i.properties, defs);
	let mut facts = vec![
		("State".to_string(), state_badge(i.state)),
		("Author".to_string(), ui::mono(author_text(&i.author))),
		("Opened".to_string(), json!(date(i.created_at))),
		("Updated".to_string(), json!(date(i.updated_at))),
	];
	if let Some(project) = &i.project {
		facts.push(("Project".to_string(), ui::mono(project)));
	}
	let mut long_texts = Vec::new();
	for def in defs.iter().filter(|d| d.kind.is_some()) {
		let value = shown.get(&def.name);
		if def.kind == Some(Kind::LongText) {
			if let Some(Value::String(md)) = value {
				long_texts.push(ui::section(def.title(), vec![ui::markdown(md)]));
			}
		} else {
			facts.push((def.title().to_string(), property_node(def, value, true)));
		}
	}
	let body = if i.body.is_empty() {
		ui::text_toned("No description.", Tone::Muted)
	} else {
		ui::markdown(&i.body)
	};
	let mut children = vec![
		ui::heading(&format!("#{} {}", i.number, i.title), 2),
		ui::kv(facts),
		body,
	];
	children.extend(long_texts);
	ui::doc(ui::stack(children))
}

/// The tab action's result: a toast and a re-render of the tab.
pub fn created_result(issue: &Issue) -> Value {
	let mut r = ui::result::toast(Tone::Success, &format!("Opened #{}", issue.number));
	r["refresh"] = json!(["issues"]);
	r
}

/// A refused form submit, shown as a toast instead of an error.
pub fn refused_result(message: &str) -> Value {
	ui::result::toast(Tone::Danger, message)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn author() -> Author {
		Author {
			kind: "user".into(),
			id: "usr_dev".into(),
			on_behalf_of: None,
		}
	}

	fn issue(number: u32, state: State) -> Issue {
		Issue {
			number,
			title: format!("Issue {number}"),
			body: "body".into(),
			project: None,
			state,
			author: author(),
			created_at: 1_700_000_000_000,
			updated_at: 1_762_128_000_000,
			closed_at: None,
			properties: Map::new(),
			version: 1,
		}
	}

	#[test]
	fn new_issues_are_trimmed_and_bounded() {
		assert_eq!(
			new_issue(&json!({ "title": "  Crash on push ", "body": "steps\n\n" }), &[]).unwrap(),
			NewIssue {
				title: "Crash on push".into(),
				body: "steps".into(),
				properties: Map::new(),
			}
		);
		assert_eq!(new_issue(&json!({ "title": "x" }), &[]).unwrap().body, "");
		for bad in [
			json!({}),
			json!({ "title": "   " }),
			json!({ "title": 3 }),
			json!({ "title": "a\nb" }),
			json!({ "title": "x".repeat(TITLE_MAX_CHARS + 1) }),
			json!({ "title": "x", "body": 1 }),
			json!({ "title": "x", "body": "b".repeat(BODY_MAX_CHARS + 1) }),
			json!({ "title": "x", "properties": [] }),
		] {
			assert!(matches!(new_issue(&bad, &[]), Err(Error::Invalid(_))), "{bad}");
		}
		// Characters, not bytes.
		assert!(new_issue(&json!({ "title": "é".repeat(TITLE_MAX_CHARS) }), &[]).is_ok());
	}

	#[test]
	fn numbers_and_list_arguments() {
		assert_eq!(number_arg(&json!({ "number": 7 })).unwrap(), 7);
		for bad in [
			json!({}),
			json!({ "number": 0 }),
			json!({ "number": -1 }),
			json!({ "number": "7" }),
		] {
			assert!(matches!(number_arg(&bad), Err(Error::Invalid(_))), "{bad}");
		}
		assert!(number_arg(&json!({ "number": u64::from(NUMBER_MAX) + 1 })).is_err());
		assert_eq!(
			list_args(&json!({})).unwrap(),
			(Filter::Only(State::Open), LIST_DEFAULT_LIMIT)
		);
		assert_eq!(
			list_args(&json!({ "state": "closed", "limit": 5 })).unwrap(),
			(Filter::Only(State::Closed), 5)
		);
		assert_eq!(list_args(&json!({ "state": "all" })).unwrap().0, Filter::All);
		for bad in [
			json!({ "state": "merged" }),
			json!({ "state": 1 }),
			json!({ "limit": 0 }),
			json!({ "limit": 101 }),
		] {
			assert!(matches!(list_args(&bad), Err(Error::Invalid(_))), "{bad}");
		}
	}

	#[test]
	fn the_author_is_the_calls_actor() {
		let agent = Actor {
			kind: ActorKind::Agent,
			id: "agt_1".into(),
			on_behalf_of: Some("usr_dev".into()),
		};
		let a = author_of(Some(&agent)).unwrap();
		assert_eq!(a.kind, "agent");
		assert_eq!(a.on_behalf_of.as_deref(), Some("usr_dev"));
		assert!(matches!(author_of(None), Err(Error::Denied(_))));
	}

	#[test]
	fn issue_json_has_the_body_only_when_asked() {
		let i = issue(3, State::Open);
		let full = issue_json(&i, true, &[]);
		assert_eq!(full["number"], 3);
		assert_eq!(full["state"], "open");
		assert_eq!(full["project"], Value::Null);
		assert_eq!(full["author"], json!({ "kind": "user", "id": "usr_dev" }));
		assert_eq!(full["body"], "body");
		assert_eq!(full["closedAt"], Value::Null);
		assert_eq!(full["properties"], json!({}));
		assert!(full.get("fields").is_none());
		assert!(issue_json(&i, false, &[]).get("body").is_none());
		let scoped = Issue {
			project: Some("crates/lsp".into()),
			..i
		};
		assert_eq!(issue_json(&scoped, false, &[])["project"], "crates/lsp");
	}

	#[test]
	fn a_project_is_a_clean_root_path() {
		assert_eq!(project_arg(&json!({})).unwrap(), None);
		assert_eq!(project_arg(&json!({ "project": null })).unwrap(), None);
		assert_eq!(
			project_arg(&json!({ "project": "crates/lsp" }))
				.unwrap()
				.as_deref(),
			Some("crates/lsp")
		);
		for odd in ["back\\slash", "new\nline", "tab\there"] {
			assert_eq!(
				project_arg(&json!({ "project": odd })).unwrap().as_deref(),
				Some(odd)
			);
		}
		for bad in [
			json!({ "project": "" }),
			json!({ "project": 1 }),
			json!({ "project": "/crates" }),
			json!({ "project": "crates/" }),
			json!({ "project": "crates//lsp" }),
			json!({ "project": "../etc" }),
			json!({ "project": "a/./b" }),
			json!({ "project": "a\0b" }),
			json!({ "project": "x".repeat(PROJECT_MAX_BYTES + 1) }),
		] {
			assert!(matches!(project_arg(&bad), Err(Error::Invalid(_))), "{bad}");
		}
	}

	#[test]
	fn dates_are_utc_days() {
		assert_eq!(date(0), "1970-01-01");
		assert_eq!(date(951_782_400_000), "2000-02-29");
		assert_eq!(date(1_762_128_000_000), "2025-11-03");
	}

	#[test]
	fn the_tab_lists_issues_and_has_a_form() {
		let doc = tab_doc(&[issue(2, State::Closed), issue(1, State::Open)], 1, 1, &[]);
		let children = &doc["root"]["children"];
		assert_eq!(children[0]["text"], "Issues");
		assert_eq!(children[1]["children"][0]["value"], 1.0);
		assert_eq!(children[2]["t"], "table");
		assert_eq!(children[2]["rows"][0][0], "#2");
		assert_eq!(children[2]["rows"][0][2], "—");
		assert_eq!(children[2]["rows"][0][3]["text"], "closed");
		assert_eq!(children[2]["rows"][1][3]["tone"], "success");
		assert_eq!(children[2]["rows"][1][5], "2025-11-03");
		let form = &children[3]["children"][0];
		assert_eq!(form["t"], "form");
		assert_eq!(form["submit"]["action"]["id"], CREATE_ACTION);
		assert_eq!(form["fields"][0]["name"], FORM_TITLE);
		let empty = tab_doc(&[], 0, 0, &[]);
		assert_eq!(empty["root"]["children"][2]["t"], "empty");
	}

	#[test]
	fn action_results() {
		let r = created_result(&issue(4, State::Open));
		assert_eq!(r["toast"]["text"], "Opened #4");
		assert_eq!(r["refresh"], json!(["issues"]));
		assert_eq!(refused_result("title is required")["toast"]["tone"], "danger");
	}

	fn defs() -> Vec<PropertyDef> {
		property_defs(&json!({ "properties": {
			"status": {
				"kind": "select", "name": "status", "options": ["todo", "done"],
				"labels": { "todo": "To do" }, "default": "todo",
			},
			"estimate": { "kind": "number", "name": "estimate" },
			"note": { "kind": "shorttext", "name": "note", "label": "Note" },
			"notes": { "kind": "longtext", "name": "notes", "label": "Notes" },
		}}))
	}

	#[test]
	fn properties_are_listed_as_configured() {
		let listed = defs_json(&defs());
		assert_eq!(listed[0], json!({ "name": "estimate", "kind": "number" }));
		assert_eq!(
			listed[3],
			json!({
				"name": "status", "kind": "select", "options": ["todo", "done"],
				"labels": { "todo": "To do" }, "default": "todo",
			})
		);
	}

	#[test]
	fn properties_are_checked_on_write_and_defaults_fill_on_read() {
		let defs = defs();
		assert_eq!(defs[0].name, "estimate");
		assert!(checked_properties(&json!({ "status": "done", "estimate": 3 }), &defs).is_ok());
		assert!(checked_properties(&json!({ "status": null }), &defs).is_ok());
		assert!(checked_properties(&json!({ "status": "other" }), &defs).is_err());
		assert!(checked_properties(&json!({ "estimate": "3" }), &defs).is_err());
		assert!(checked_properties(&json!({ "missing": 1 }), &defs).is_err());
		let shown = shown_properties(&Map::new(), &defs);
		assert_eq!(shown["status"], json!("todo"));
		assert!(!shown.contains_key("estimate"));
	}

	#[test]
	fn a_removed_property_stays_stored_and_shown() {
		let mut stored = Map::new();
		stored.insert("gone".into(), json!("kept"));
		assert_eq!(shown_properties(&stored, &defs())["gone"], json!("kept"));
	}

	#[test]
	fn a_change_reads_only_what_was_given() {
		let change = change_args(
			&json!({ "state": "closed", "properties": { "status": "done" }, "expectedVersion": 4 }),
			&defs(),
		)
		.unwrap();
		assert_eq!(change.state, Some(State::Closed));
		assert_eq!(change.expected_version, Some(4));
		assert_eq!(change.title, None);
		assert_eq!(change.properties["status"], json!("done"));
		assert!(change_args(&json!({ "state": "other" }), &defs()).is_err());
		assert!(change_args(&json!({ "properties": { "status": "other" } }), &defs()).is_err());
	}

	#[test]
	fn form_inputs_become_create_arguments() {
		let args = form_args(
			&json!({
				"_title": "Crash", "_body": "steps",
				"status": "done", "estimate": "2", "note": "", "notes": "# A\n\nb",
			}),
			&defs(),
		)
		.unwrap();
		assert_eq!(args["title"], "Crash");
		assert_eq!(args["body"], "steps");
		assert_eq!(args["properties"]["status"], json!("done"));
		assert_eq!(args["properties"]["estimate"], json!(2));
		assert_eq!(args["properties"]["notes"], json!("# A\n\nb"));
		assert!(args["properties"].get("note").is_none());
		assert!(new_issue(&args, &defs()).is_ok());
		assert!(form_args(&json!({ "estimate": "x" }), &defs()).is_err());
	}

	#[test]
	fn the_property_kinds_accept_what_cue_accepts() {
		let vectors: Value = serde_json::from_str(include_str!("../testdata/property-values.json")).unwrap();
		let mut kinds = Vec::new();
		for case in vectors.as_array().unwrap() {
			let defs = property_defs(&json!({ "properties": { "p": case["property"] } }));
			kinds.push(defs[0].kind.unwrap());
			for value in case["valid"].as_array().unwrap() {
				assert!(
					checked_properties(&json!({ "p": value }), &defs).is_ok(),
					"{case} {value}"
				);
			}
			for value in case["invalid"].as_array().unwrap() {
				assert!(
					checked_properties(&json!({ "p": value }), &defs).is_err(),
					"{case} {value}"
				);
			}
		}
		// The vectors cover every kind.
		for kind in [
			"shorttext",
			"longtext",
			"number",
			"date",
			"boolean",
			"select",
			"multiselect",
			"person",
			"url",
			"money",
		] {
			assert!(kinds.contains(&Kind::parse(kind).unwrap()), "{kind}");
		}
	}

	#[test]
	fn a_kind_this_version_does_not_know_holds_nothing() {
		let defs = property_defs(&json!({ "properties": { "p": { "kind": "future" } } }));
		assert!(checked_properties(&json!({ "p": "x" }), &defs).is_err());
		assert_eq!(defs_json(&defs)[0]["kind"], "future");
	}

	#[test]
	fn money_is_exact_minor_units() {
		assert_eq!(parse_money("12.50", "GBP"), Some(1250));
		assert_eq!(parse_money("12", "GBP"), Some(1200));
		assert_eq!(parse_money(".5", "GBP"), Some(50));
		assert_eq!(parse_money("-0.07", "GBP"), Some(-7));
		assert_eq!(parse_money("0.07", "GBP"), Some(7));
		assert_eq!(parse_money("500", "JPY"), Some(500));
		assert_eq!(parse_money("1.234", "KWD"), Some(1234));
		for bad in ["", ".", "-", "1.234", "1e3", "12,50", "£12", "90071992547409.92"] {
			assert_eq!(parse_money(bad, "GBP"), None, "{bad}");
		}
		assert_eq!(parse_money("1.5", "JPY"), None);
		assert_eq!(format_money(1250, "GBP"), "GBP 12.50");
		assert_eq!(format_money(-7, "GBP"), "GBP -0.07");
		assert_eq!(format_money(500, "JPY"), "JPY 500");
		assert_eq!(format_money(1234, "KWD"), "KWD 1.234");
	}

	#[test]
	fn each_kind_has_its_form_control_and_reads_its_value() {
		let defs = property_defs(&json!({ "properties": {
			"budget": { "kind": "money", "currency": "GBP" },
			"due": { "kind": "date" },
			"flag": { "kind": "boolean", "default": true },
			"link": { "kind": "url" },
			"notes": { "kind": "longtext" },
			"owner": { "kind": "person" },
			"size": { "kind": "number" },
			"tags": { "kind": "multiselect", "options": ["a", "b"], "labels": { "b": "Bee" }, "default": ["a"] },
		}}));
		let form = create_form(&defs);
		let input = |name: &str| {
			form["fields"]
				.as_array()
				.unwrap()
				.iter()
				.find(|f| f["name"] == name)
				.unwrap()
				.clone()
		};
		assert_eq!(input("budget")["label"], "budget (GBP)");
		assert_eq!(input("due")["inputType"], "date");
		assert_eq!(input("flag")["t"], "checkbox");
		assert_eq!(input("flag")["value"], true);
		assert_eq!(input("link")["inputType"], "url");
		assert_eq!(input("notes")["t"], "textarea");
		assert_eq!(input("owner")["t"], "input");
		assert_eq!(input("size")["inputType"], "number");
		assert_eq!(input("tags")["multiple"], true);
		assert_eq!(
			input("tags")["options"][1],
			json!({ "value": "b", "label": "Bee" })
		);
		assert_eq!(input("tags")["value"], json!(["a"]));
		let args = form_args(
			&json!({
				"_title": "t",
				"budget": "12.50",
				"due": "2026-11-06",
				"flag": false,
				"link": " https://example.com ",
				"notes": "line one\nline two",
				"owner": "usr_dev",
				"size": "1.5",
				"tags": ["b"],
			}),
			&defs,
		)
		.unwrap();
		let properties = &args["properties"];
		assert_eq!(
			properties,
			&json!({
				"budget": 1250, "due": "2026-11-06", "flag": false, "link": "https://example.com",
				"notes": "line one\nline two", "owner": "usr_dev", "size": 1.5, "tags": ["b"],
			})
		);
		assert!(checked_properties(properties, &defs).is_ok());
		let emptied = form_args(&json!({ "tags": [] }), &defs).unwrap();
		assert_eq!(emptied["properties"], json!({ "tags": [] }));
		assert_eq!(property_text(&defs[7], emptied["properties"].get("tags")), "—");
		assert_eq!(property_text(&defs[0], properties.get("budget")), "GBP 12.50");
		assert_eq!(property_text(&defs[2], properties.get("flag")), "No");
		assert_eq!(property_text(&defs[7], properties.get("tags")), "Bee");
	}

	#[test]
	fn the_issue_view_renders_long_text_as_markdown() {
		let defs = property_defs(&json!({ "properties": {
			"notes": { "kind": "longtext", "label": "Notes" },
			"link": { "kind": "url" },
			"owner": { "kind": "person", "label": "Owner" },
		}}));
		let mut i = issue(5, State::Open);
		i.body = "# Steps\n\n1. push".into();
		i.properties = json!({
			"notes": "- a\n- b",
			"link": "https://example.com/x",
			"owner": "usr_dev",
		})
		.as_object()
		.unwrap()
		.clone();
		let doc = issue_doc(&i, &defs);
		let children = doc["root"]["children"].as_array().unwrap();
		assert_eq!(children[0]["text"], "#5 Issue 5");
		assert_eq!(
			children[2],
			json!({ "t": "markdown", "md": "# Steps\n\n1. push" })
		);
		let notes = children.iter().find(|c| c["title"] == "Notes").unwrap();
		assert_eq!(notes["children"][0], json!({ "t": "markdown", "md": "- a\n- b" }));
		let facts = children[1]["items"].as_array().unwrap();
		let fact = |k: &str| facts.iter().find(|f| f["k"] == k).unwrap()["v"].clone();
		assert_eq!(fact("link")["t"], "link");
		assert_eq!(fact("Owner")["mono"], true);
		assert!(facts.iter().all(|f| f["k"] != "Notes"));
	}

	#[test]
	fn long_text_is_an_excerpt_in_tables() {
		let defs = property_defs(&json!({ "properties": {
			"notes": { "kind": "longtext" },
			"title2": { "kind": "shorttext" },
		}}));
		let mut i = issue(1, State::Open);
		i.properties.insert("notes".into(), json!("a\nb"));
		let doc = tab_doc(&[i], 1, 0, &defs);
		let table = &doc["root"]["children"][2];
		assert_eq!(
			table["columns"],
			json!(["#", "Title", "title2", "Project", "State", "Author", "Updated"])
		);
		assert_eq!(excerpt("a\nb", 10), "a…");
		assert_eq!(excerpt("abc", 2), "ab…");
		assert_eq!(excerpt("abc", 3), "abc");
	}
}
