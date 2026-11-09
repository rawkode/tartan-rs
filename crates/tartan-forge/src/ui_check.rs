//! The host's check of the `person` nodes in an extension's `tartan-ui@1` document (design D14).
//!
//! A `person` node is `{"t": "person", "did": …}`. The web UI resolves its DID to a handle with
//! `GET /-/api/people`, so the host refuses a document whose `person` node does not name a DID
//! Tartan accepts, rather than pass an arbitrary string to the people lookup.

use serde_json::Value;
use tartan_atproto::Mode;
use tartan_atproto::did::Did;

/// The deepest document the check walks; `tartan-ui@1` documents are at most 16 levels deep,
/// and the extra room covers the wrappers (`render`, `root`, `children`, cells) around them.
const MAX_DEPTH: usize = 64;

/// Checks every `person` node in `value`, an extension's `render` or `on-action` result. The
/// error names the first offending node.
pub fn check_person_nodes(value: &Value, mode: Mode) -> Result<(), String> {
    walk(value, mode, 0)
}

fn walk(value: &Value, mode: Mode, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("the document is nested too deeply".to_owned());
    }
    match value {
        Value::Object(map) => {
            if map.get("t").and_then(Value::as_str) == Some("person") {
                let did = map
                    .get("did")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "a person node has no did".to_owned())?;
                Did::parse(did, mode)
                    .map_err(|error| format!("a person node's did {did:?} is invalid: {error}"))?;
            }
            map.values()
                .try_for_each(|child| walk(child, mode, depth + 1))
        }
        Value::Array(items) => items
            .iter()
            .try_for_each(|item| walk(item, mode, depth + 1)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_valid_did_passes() {
        let doc = json!({"v": 1, "root": {"t": "stack", "children": [
            {"t": "person", "did": "did:plc:ewvi7nxzyoun6zhxrhs64oiz"},
            {"t": "kv", "items": [{"k": "Owner", "v": {"t": "person", "did": "did:web:rawkode.dev"}}]},
        ]}});
        assert_eq!(check_person_nodes(&doc, Mode::Production), Ok(()));
    }

    #[test]
    fn an_invalid_did_is_refused() {
        for did in [
            json!("admin"),
            json!("did:plc:short"),
            json!("did:web:a.example:path"),
            json!(7),
        ] {
            let doc = json!({"v": 1, "root": {"t": "table", "columns": ["Who"],
                "rows": [[{"t": "person", "did": did}]]}});
            assert!(
                check_person_nodes(&doc, Mode::Production).is_err(),
                "{did} should be refused"
            );
        }
        let missing = json!({"render": {"v": 1, "root": {"t": "person"}}});
        assert!(check_person_nodes(&missing, Mode::Production).is_err());
    }

    #[test]
    fn a_localhost_port_is_refused_outside_development() {
        let doc = json!({"t": "person", "did": "did:web:localhost%3A9400"});
        assert!(check_person_nodes(&doc, Mode::Production).is_err());
    }

    #[test]
    fn other_nodes_and_text_are_untouched() {
        let doc = json!({"v": 1, "root": {"t": "text", "text": "person"}, "person": "admin"});
        assert_eq!(check_person_nodes(&doc, Mode::Production), Ok(()));
    }
}
