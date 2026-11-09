//! DIDs, the way `tartan.rawkode.academy/types` (`#DID`) spells them: Tartan
//! names every person by DID and shows them by handle.
//!
//! A DID is `did:plc:` and 24 characters of `a-z` and `2-7`, or `did:web:`
//! and a lowercase hostname of at most 253 characters with no path. Only
//! `localhost` may carry a port, written `%3A` (`did:web:localhost%3A9400`),
//! which ATProto allows for local testing only; the forge itself accepts that
//! form only with its development identity network.

/// The longest DID `#DID` accepts, in characters.
pub const MAX_CHARS: usize = 261;

/// Whether `text` is a DID as `#DID` defines it.
pub fn is_did(text: &str) -> bool {
	if text.chars().count() > MAX_CHARS {
		return false;
	}
	if let Some(id) = text.strip_prefix("did:plc:") {
		return id.len() == 24
			&& id
				.bytes()
				.all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
	}
	let Some(host) = text.strip_prefix("did:web:") else {
		return false;
	};
	if let Some(rest) = host.strip_prefix("localhost") {
		if rest.is_empty() {
			return true;
		}
		if let Some(port) = rest.strip_prefix("%3A") {
			return (1..=5).contains(&port.len())
				&& port.bytes().all(|b| b.is_ascii_digit())
				&& !port.starts_with('0');
		}
	}
	is_hostname(host)
}

/// A lowercase hostname with at least two labels, the last starting with a
/// letter.
fn is_hostname(host: &str) -> bool {
	let labels: Vec<&str> = host.split('.').collect();
	labels.len() >= 2
		&& labels.iter().all(|label| is_label(label))
		&& labels
			.last()
			.is_some_and(|tld| tld.starts_with(|c: char| c.is_ascii_lowercase()))
}

fn is_label(label: &str) -> bool {
	let bytes = label.as_bytes();
	let inner = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-';
	let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
	(1..=63).contains(&bytes.len())
		&& bytes.iter().all(inner)
		&& bytes.first().is_some_and(edge)
		&& bytes.last().is_some_and(edge)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn valid_dids() {
		for did in [
			"did:plc:ewvi7nxzyoun6zhxrhs64oiz",
			"did:web:rawkode.dev",
			"did:web:a-b.example.com",
			"did:web:localhost",
			"did:web:localhost%3A9400",
		] {
			assert!(is_did(did), "{did}");
		}
	}

	#[test]
	fn invalid_dids() {
		for did in [
			"",
			"admin",
			"usr_dev",
			"alice.bsky.social",
			"did:plc:ewvi7nxzyoun6zhxrhs64oi",
			"did:plc:EWVI7NXZYOUN6ZHXRHS64OIZ",
			"did:plc:ewvi7nxzyoun6zhxrhs64oi1",
			"did:web:Rawkode.dev",
			"did:web:rawkode",
			"did:web:rawkode.dev:path",
			"did:web:rawkode.dev%3A443",
			"did:web:localhost%3A0",
			"did:web:localhost%3A123456",
			"did:web:-a.example",
			"did:web:a.1example",
			"did:key:z6Mk",
		] {
			assert!(!is_did(did), "{did}");
		}
	}
}
