//! Builders for `tartan-ui@1`, the same nodes as
//! `@tartan/ext-api`'s `ui`. A render returns `ui::doc(root).to_string()`.
//!
//! The host validates every document (≤ 500 nodes, ≤ 64 KB, depth ≤ 16;
//! links are same-origin paths `/…` or `https://` URLs; no HTML) and shows an
//! error chip instead of an invalid one.

use serde_json::{json, Map, Value};

/// A `tartan-ui@1` node.
pub type Node = Value;

/// Semantic tones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
	Neutral,
	Info,
	Success,
	Warning,
	Danger,
	Muted,
}

impl Tone {
	pub fn as_str(self) -> &'static str {
		match self {
			Tone::Neutral => "neutral",
			Tone::Info => "info",
			Tone::Success => "success",
			Tone::Warning => "warning",
			Tone::Danger => "danger",
			Tone::Muted => "muted",
		}
	}
}

fn node(t: &str, fields: Vec<(&str, Value)>) -> Node {
	let mut map = Map::new();
	map.insert("t".to_string(), Value::String(t.to_string()));
	for (key, value) in fields {
		if !value.is_null() {
			map.insert(key.to_string(), value);
		}
	}
	Value::Object(map)
}

/// A document.
pub fn doc(root: Node) -> Value {
	json!({ "v": 1, "root": root })
}

/// A document the host re-renders when one of the event patterns arrives.
pub fn doc_refreshing(root: Node, refresh_on: &[&str]) -> Value {
	json!({ "v": 1, "root": root, "refreshOn": refresh_on })
}

pub fn stack(children: Vec<Node>) -> Node {
	node("stack", vec![("children", Value::Array(children))])
}

pub fn row(children: Vec<Node>) -> Node {
	node("row", vec![("children", Value::Array(children))])
}

pub fn grid(children: Vec<Node>, cols: u8) -> Node {
	node(
		"grid",
		vec![("children", Value::Array(children)), ("cols", json!(cols))],
	)
}

pub fn card(children: Vec<Node>) -> Node {
	node("card", vec![("children", Value::Array(children))])
}

pub fn section(title: &str, children: Vec<Node>) -> Node {
	node(
		"section",
		vec![("title", json!(title)), ("children", Value::Array(children))],
	)
}

pub fn divider() -> Node {
	node("divider", vec![])
}

/// A heading of level 2, 3 or 4.
pub fn heading(text: &str, level: u8) -> Node {
	node(
		"heading",
		vec![("text", json!(text)), ("level", json!(level.clamp(2, 4)))],
	)
}

pub fn text(text: impl Into<String>) -> Node {
	node("text", vec![("text", Value::String(text.into()))])
}

pub fn text_toned(text: impl Into<String>, tone: Tone) -> Node {
	node(
		"text",
		vec![
			("text", Value::String(text.into())),
			("tone", json!(tone.as_str())),
		],
	)
}

/// Monospace text.
pub fn mono(text: impl Into<String>) -> Node {
	node(
		"text",
		vec![("text", Value::String(text.into())), ("mono", json!(true))],
	)
}

pub fn label(text: impl Into<String>) -> Node {
	node("label", vec![("text", Value::String(text.into()))])
}

pub fn badge(text: impl Into<String>, tone: Tone) -> Node {
	node(
		"badge",
		vec![
			("text", Value::String(text.into())),
			("tone", json!(tone.as_str())),
		],
	)
}

/// An empty state, with an optional explanation.
pub fn empty(text: impl Into<String>, body: Option<&str>) -> Node {
	node(
		"empty",
		vec![("text", Value::String(text.into())), ("body", json!(body))],
	)
}

/// Markdown, rendered by the host without raw HTML.
pub fn markdown(md: impl Into<String>) -> Node {
	node("markdown", vec![("md", Value::String(md.into()))])
}

pub fn code(text: impl Into<String>, lang: Option<&str>) -> Node {
	node(
		"code",
		vec![("text", Value::String(text.into())), ("lang", json!(lang))],
	)
}

/// `href`: a same-origin path (`/…`) or an `https://` URL.
pub fn link(text: impl Into<String>, href: impl Into<String>) -> Node {
	node(
		"link",
		vec![
			("text", Value::String(text.into())),
			("href", Value::String(href.into())),
		],
	)
}

pub fn stat(label: &str, value: f64) -> Node {
	node("stat", vec![("label", json!(label)), ("value", json!(value))])
}

/// Key/value pairs; a value is text or a node.
pub fn kv(items: Vec<(String, Node)>) -> Node {
	let items: Vec<Value> = items
		.into_iter()
		.map(|(k, v)| json!({ "k": k, "v": v }))
		.collect();
	node("kv", vec![("items", Value::Array(items))])
}

pub fn alert(tone: Tone, title: &str, body: Option<Node>) -> Node {
	node(
		"alert",
		vec![
			("tone", json!(tone.as_str())),
			("title", json!(title)),
			("body", body.unwrap_or(Value::Null)),
		],
	)
}

/// A table; a cell is text, a number or a node.
pub fn table(columns: &[&str], rows: Vec<Vec<Node>>) -> Node {
	let rows: Vec<Value> = rows.into_iter().map(Value::Array).collect();
	node(
		"table",
		vec![("columns", json!(columns)), ("rows", Value::Array(rows))],
	)
}

pub fn list(items: Vec<Node>) -> Node {
	node("list", vec![("items", Value::Array(items))])
}

/// A button or menu action: `id` matches `[a-z0-9._-]{1,64}`.
pub fn action(id: &str, payload: Option<Value>, confirm: Option<&str>) -> Value {
	let mut map = Map::new();
	map.insert("id".to_string(), json!(id));
	if let Some(payload) = payload {
		map.insert("payload".to_string(), payload);
	}
	if let Some(confirm) = confirm {
		map.insert("confirm".to_string(), json!(confirm));
	}
	Value::Object(map)
}

pub fn button(text: &str, action: Value, tone: Option<Tone>) -> Node {
	node(
		"button",
		vec![
			("text", json!(text)),
			("action", action),
			("tone", json!(tone.map(Tone::as_str))),
		],
	)
}

/// A form. On submit the host posts `submit` (an [`action`]) with the field
/// values by name at the top level of the payload, the action's own payload
/// merged over them: `on_action` reads `payload["title"]`.
pub fn form(fields: Vec<Node>, submit_text: &str, submit: Value) -> Node {
	node(
		"form",
		vec![
			("fields", Value::Array(fields)),
			("submit", json!({ "text": submit_text, "action": submit })),
		],
	)
}

fn field(t: &str, name: &str, label: Option<&str>, required: bool) -> Node {
	node(
		t,
		vec![
			("name", json!(name)),
			("label", json!(label)),
			("required", if required { json!(true) } else { Value::Null }),
		],
	)
}

/// A one-line text field of a [`form`]; `name` matches `[a-z0-9_]{1,32}`.
pub fn input(name: &str, label: Option<&str>, required: bool) -> Node {
	field("input", name, label, required)
}

/// A multi-line text field of a [`form`].
pub fn textarea(name: &str, label: Option<&str>, required: bool) -> Node {
	field("textarea", name, label, required)
}

/// `on-action` results.
pub mod result {
	use super::Tone;
	use serde_json::{json, Value};

	pub fn ok() -> Value {
		json!({ "v": 1 })
	}

	pub fn toast(tone: Tone, text: &str) -> Value {
		json!({ "v": 1, "toast": { "tone": tone.as_str(), "text": text } })
	}

	pub fn render(doc: Value) -> Value {
		json!({ "v": 1, "render": doc })
	}

	/// `path` must be same-origin (`/…`).
	pub fn navigate(path: &str) -> Value {
		json!({ "v": 1, "navigate": path })
	}

	pub fn refresh(slot_ids: &[&str]) -> Value {
		json!({ "v": 1, "refresh": slot_ids })
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn nodes_drop_absent_optional_props() {
		assert_eq!(empty("Nothing", None), json!({ "t": "empty", "text": "Nothing" }));
		assert_eq!(
			empty("Nothing", Some("why")),
			json!({ "t": "empty", "text": "Nothing", "body": "why" })
		);
		assert_eq!(code("x", None), json!({ "t": "code", "text": "x" }));
		assert_eq!(
			button("Go", action("go", None, None), None),
			json!({ "t": "button", "text": "Go", "action": { "id": "go" } })
		);
	}

	#[test]
	fn a_document_with_containers_and_cells() {
		let d = doc(section(
			"Secrets",
			vec![
				badge("2 found", Tone::Danger),
				table(&["Path", "Line"], vec![vec![json!("a.env"), json!(3)]]),
				kv(vec![("Verdict".into(), json!("veto"))]),
			],
		));
		assert_eq!(d["v"], 1);
		assert_eq!(d["root"]["t"], "section");
		assert_eq!(d["root"]["title"], "Secrets");
		assert_eq!(
			d["root"]["children"][0],
			json!({ "t": "badge", "text": "2 found", "tone": "danger" })
		);
		assert_eq!(d["root"]["children"][1]["rows"], json!([["a.env", 3]]));
		assert_eq!(
			d["root"]["children"][2]["items"],
			json!([{ "k": "Verdict", "v": "veto" }])
		);
	}

	#[test]
	fn heading_levels_are_clamped_and_refresh_is_kept() {
		assert_eq!(heading("H", 9)["level"], 4);
		assert_eq!(heading("H", 1)["level"], 2);
		assert_eq!(
			doc_refreshing(text("x"), &["gate.decided"])["refreshOn"],
			json!(["gate.decided"])
		);
		assert_eq!(result::toast(Tone::Success, "done")["toast"]["tone"], "success");
	}

	#[test]
	fn a_form_with_fields() {
		let f = form(
			vec![input("title", Some("Title"), true), textarea("body", None, false)],
			"Create",
			action("create", None, None),
		);
		assert_eq!(
			f,
			json!({
				"t": "form",
				"fields": [
					{ "t": "input", "name": "title", "label": "Title", "required": true },
					{ "t": "textarea", "name": "body" },
				],
				"submit": { "text": "Create", "action": { "id": "create" } },
			})
		);
	}
}
