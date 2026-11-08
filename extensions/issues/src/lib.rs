//! `rawkode.issues`: per-repository issues for Tartan, written in Rust and
//! run as a WebAssembly component. An issue may belong to a project: every
//! tool takes an optional `project`, the project's root path. The extension
//! cannot see the project graph; the kernel's JSON route resolves a project's
//! name to that path and refuses a project with the extension switched off.
//!
//! - tools (`issues_<name>` over MCP): `list`, `get`, `fields`, `create`,
//!   `update`, `close`, `reopen`, also exported to other extensions as
//!   `rawkode.issues/issues@1`;
//! - fields from the repository's `tartan.cue` (`settings.fields`): stored per
//!   issue, checked when written, and never rewritten when the config changes;
//! - the `repo.tab` Issues page: counts, the newest issues and a form that
//!   opens one (action `create`).
//!
//! It needs no permission: everything lives in its own SQLite, one database
//! per repository (`storage.scope: "repo"`), and the author of an issue is
//! the actor the host gives the call.

pub mod issue;
pub mod store;

use serde_json::{json, Value};
use tartan_ext::serde_json;
use tartan_ext::{export, parse_json, Actor, Error, Extension, Json, SlotContext};

use issue::{Change, FieldDef, Issue, State, CREATE_ACTION, TAB_LIMIT};

fn not_found(number: u32) -> Error {
	Error::not_found(format!("issue #{number}"))
}

/// The issue called for, unless a `project` was given and it is another's.
fn scoped(args: &Value) -> Result<Issue, Error> {
	let number = issue::number_arg(args)?;
	let project = issue::project_arg(args)?;
	store::get(number)?
		.filter(|found| project.is_none() || found.project == project)
		.ok_or_else(|| not_found(number))
}

fn defs() -> Vec<FieldDef> {
	issue::field_defs(&tartan_ext::host::config())
}

fn change(args: &Value, change: &Change, defs: &[FieldDef]) -> Result<Value, Error> {
	let number = scoped(args)?.number;
	let (issue, changed) = store::apply(number, change)?.ok_or_else(|| not_found(number))?;
	Ok(json!({ "issue": issue::issue_json(&issue, false, defs), "changed": changed }))
}

fn state_change(state: State) -> Change {
	Change {
		state: Some(state),
		..Change::default()
	}
}

/// A tool call; `actor` is the caller.
pub fn tool(name: &str, args: &Value, actor: Option<&Actor>) -> Result<Value, Error> {
	let defs = defs();
	match name {
		"list" => {
			let (filter, limit) = issue::list_args(args)?;
			let project = issue::project_arg(args)?;
			let before = issue::before_arg(args)?;
			let issues = store::list(filter, project.as_deref(), before, limit)?;
			let (open, closed) = store::counts(project.as_deref())?;
			let next_before = (issues.len() == limit as usize)
				.then(|| issues.last().map(|i| i.number))
				.flatten();
			Ok(json!({
				"state": filter.as_str(),
				"project": project,
				"issues": issues.iter().map(|i| issue::issue_json(i, false, &defs)).collect::<Vec<_>>(),
				"open": open,
				"closed": closed,
				"fields": issue::defs_json(&defs),
				"nextBefore": next_before,
			}))
		}
		"get" => Ok(json!({ "issue": issue::issue_json(&scoped(args)?, true, &defs) })),
		"fields" => Ok(json!({ "fields": issue::defs_json(&defs) })),
		"create" => {
			let new = issue::new_issue(args, &defs)?;
			let project = issue::project_arg(args)?;
			let created = store::create(&new, project.as_deref(), &issue::author_of(actor)?)?;
			Ok(json!({ "issue": issue::issue_json(&created, true, &defs) }))
		}
		"update" => change(args, &issue::change_args(args, &defs)?, &defs),
		"close" => change(args, &state_change(State::Closed), &defs),
		"reopen" => change(args, &state_change(State::Open), &defs),
		_ => Err(Error::not_found(format!("tool {name}"))),
	}
}

struct Issues;

impl Extension for Issues {
	fn render(slot: String, _ctx: SlotContext, _props: Json) -> Result<Json, Error> {
		if slot != "issues" {
			return Err(Error::not_found(format!("slot {slot}")));
		}
		let (open, closed) = store::counts(None)?;
		let issues = store::list(issue::Filter::All, None, None, TAB_LIMIT)?;
		Ok(issue::tab_doc(&issues, open, closed, &defs()).to_string())
	}

	fn on_action(action: String, payload: Json, ctx: SlotContext) -> Result<Json, Error> {
		if action != CREATE_ACTION {
			return Err(Error::not_found(format!("action {action}")));
		}
		let defs = defs();
		let mut form = parse_json(&payload)?;
		let parsed = issue::form_fields(&form, &defs).and_then(|fields| {
			form["fields"] = fields;
			issue::new_issue(&form, &defs)
		});
		let new = match parsed {
			Ok(new) => new,
			Err(Error::Invalid(message)) => return Ok(issue::refused_result(&message).to_string()),
			Err(e) => return Err(e),
		};
		let created = store::create(&new, None, &issue::author_of(ctx.viewer.as_ref())?)?;
		Ok(issue::created_result(&created).to_string())
	}

	fn call_tool(name: String, args: Json, ctx: SlotContext) -> Result<Json, Error> {
		Ok(tool(&name, &parse_json(&args)?, ctx.viewer.as_ref())?.to_string())
	}
}

export!(Issues);
