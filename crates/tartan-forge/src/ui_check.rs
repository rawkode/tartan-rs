//! The host's check of the `person` nodes in an extension's `tartan-ui@1` document (design D14).
//!
//! A `person` node is `{"t": "person", "did": …}`. The web UI resolves its DID to a handle with
//! `GET /-/api/people`, so the host refuses a document whose `person` node does not name a DID
//! Tartan accepts, rather than pass an arbitrary string to the people lookup.
//!
//! The SDK and `types.#DID` accept `did:web:localhost` DIDs (with a port) in every build, since
//! development forges use them, while a production forge does not resolve them. Data stored
//! with such a DID (an assignee, say) is still rendered: the host turns its `person` node into
//! a plain `text` node showing the DID, so one value cannot fail a whole view.

use serde_json::Value;
use tartan_atproto::Mode;
use tartan_atproto::did::Did;

/// The deepest document the check walks; `tartan-ui@1` documents are at most 16 levels deep,
/// and the extra room covers the wrappers (`render`, `root`, `children`, cells) around them.
const MAX_DEPTH: usize = 64;

/// Checks every `person` node in `value`, an extension's `render` or `on-action` result,
/// turning a localhost `did:web` node that `mode` does not accept into a `text` node. The error
/// names the first other offending node.
pub fn check_person_nodes(value: &mut Value, mode: Mode) -> Result<(), String> {
    walk(value, mode, 0)
}

/// Whether `did` is a `did:web` DID of `localhost` or a name under it, with or without a port.
fn is_localhost_web(did: &str) -> bool {
    did.strip_prefix("did:web:").is_some_and(|ident| {
        let host = ident.split_once("%3A").map_or(ident, |(host, _)| host);
        host == "localhost" || host.ends_with(".localhost")
    })
}

fn walk(value: &mut Value, mode: Mode, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("the document is nested too deeply".to_owned());
    }
    match value {
        Value::Object(map) => {
            if map.get("t").and_then(Value::as_str) == Some("person") {
                let did = map
                    .get("did")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "a person node has no did".to_owned())?
                    .to_owned();
                if let Err(error) = Did::parse(&did, mode) {
                    if is_localhost_web(&did) {
                        *value = serde_json::json!({"t": "text", "text": did});
                        return Ok(());
                    }
                    return Err(format!("a person node's did {did:?} is invalid: {error}"));
                }
            }
            map.values_mut()
                .try_for_each(|child| walk(child, mode, depth + 1))
        }
        Value::Array(items) => items
            .iter_mut()
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
        let mut checked = doc.clone();
        assert_eq!(check_person_nodes(&mut checked, Mode::Production), Ok(()));
        assert_eq!(checked, doc);
    }

    #[test]
    fn an_invalid_did_is_refused() {
        for did in [
            json!("admin"),
            json!("did:plc:short"),
            json!("did:web:a.example:path"),
            json!(7),
        ] {
            let mut doc = json!({"v": 1, "root": {"t": "table", "columns": ["Who"],
                "rows": [[{"t": "person", "did": did}]]}});
            assert!(
                check_person_nodes(&mut doc, Mode::Production).is_err(),
                "{did} should be refused"
            );
        }
        let mut missing = json!({"render": {"v": 1, "root": {"t": "person"}}});
        assert!(check_person_nodes(&mut missing, Mode::Production).is_err());
    }

    #[test]
    fn a_localhost_did_is_shown_as_text_outside_development() {
        for did in [
            "did:web:localhost%3A9400",
            "did:web:localhost",
            "did:web:alice.localhost%3A9400",
        ] {
            let mut doc = json!({"v": 1, "root": {"t": "stack", "children": [
                {"t": "person", "did": did},
                {"t": "person", "did": "did:web:rawkode.dev"},
            ]}});
            assert_eq!(check_person_nodes(&mut doc, Mode::Production), Ok(()));
            assert_eq!(
                doc["root"]["children"],
                json!([
                    {"t": "text", "text": did},
                    {"t": "person", "did": "did:web:rawkode.dev"},
                ])
            );
        }
    }

    #[test]
    fn other_nodes_and_text_are_untouched() {
        let mut doc = json!({"v": 1, "root": {"t": "text", "text": "person"}, "person": "admin"});
        assert_eq!(check_person_nodes(&mut doc, Mode::Production), Ok(()));
    }
}
