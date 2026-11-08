//! Gate decisions: `allow`, `advise` or `veto`, with
//! line annotations. A gate that saw a truncated input and still scanned the
//! whole change says so with [`Decision::full_scan`]; otherwise the kernel
//! applies the gate's `onTruncated` (veto by default for `ref.advance`).

use crate::{GateDecision, Verdict};
use serde_json::json;

/// One annotated line of a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
	pub path: String,
	pub line: u32,
	pub text: String,
}

/// A decision under construction.
#[derive(Clone, Debug)]
pub struct Decision {
	verdict: Verdict,
	message: String,
	annotations: Vec<Annotation>,
	full_scan: bool,
}

/// The kernel's limits (`GateDecisionSchema`): a longer message or more
/// annotations would make the decision invalid, so they are cut here.
pub const MESSAGE_MAX_CHARS: usize = 2000;
pub const ANNOTATIONS_MAX: usize = 200;
pub const ANNOTATION_TEXT_MAX_CHARS: usize = 500;

fn cut(text: &str, max: usize) -> String {
	text.chars().take(max).collect()
}

impl Decision {
	pub fn new(verdict: Verdict, message: impl Into<String>) -> Self {
		Decision {
			verdict,
			message: message.into(),
			annotations: Vec::new(),
			full_scan: false,
		}
	}
	pub fn allow(message: impl Into<String>) -> Self {
		Decision::new(Verdict::Allow, message)
	}
	pub fn advise(message: impl Into<String>) -> Self {
		Decision::new(Verdict::Advise, message)
	}
	pub fn veto(message: impl Into<String>) -> Self {
		Decision::new(Verdict::Veto, message)
	}
	pub fn annotate(mut self, path: impl Into<String>, line: u32, text: impl Into<String>) -> Self {
		self.annotations.push(Annotation {
			path: path.into(),
			line,
			text: text.into(),
		});
		self
	}
	/// The gate scanned the whole change although its input was truncated.
	pub fn full_scan(mut self, full: bool) -> Self {
		self.full_scan = full;
		self
	}
	pub fn build(self) -> GateDecision {
		let annotations: Vec<serde_json::Value> = self
			.annotations
			.iter()
			.take(ANNOTATIONS_MAX)
			.map(|a| {
				json!({
					"path": a.path,
					"line": a.line,
					"text": cut(&a.text, ANNOTATION_TEXT_MAX_CHARS),
				})
			})
			.collect();
		GateDecision {
			verdict: self.verdict,
			message: cut(&self.message, MESSAGE_MAX_CHARS),
			annotations: serde_json::Value::Array(annotations).to_string(),
			full_scan: self.full_scan,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn builds_annotations_as_a_json_array() {
		let d = Decision::veto("no")
			.annotate("a.env", 3, "key")
			.full_scan(true)
			.build();
		assert!(matches!(d.verdict, Verdict::Veto));
		assert!(d.full_scan);
		let parsed: serde_json::Value = serde_json::from_str(&d.annotations).unwrap();
		assert_eq!(parsed, json!([{ "path": "a.env", "line": 3, "text": "key" }]));
	}

	#[test]
	fn cuts_to_the_kernel_limits() {
		let mut d = Decision::allow("x".repeat(3000));
		for i in 0..250 {
			d = d.annotate("p", i, "y".repeat(600));
		}
		let built = d.build();
		assert_eq!(built.message.chars().count(), MESSAGE_MAX_CHARS);
		let parsed: Vec<serde_json::Value> = serde_json::from_str(&built.annotations).unwrap();
		assert_eq!(parsed.len(), ANNOTATIONS_MAX);
		assert_eq!(
			parsed[0]["text"].as_str().unwrap().len(),
			ANNOTATION_TEXT_MAX_CHARS
		);
		assert_eq!(built.annotations.contains("\"line\":0"), true);
	}

	#[test]
	fn empty_annotations_are_an_empty_array() {
		assert_eq!(Decision::allow("ok").build().annotations, "[]");
	}
}
