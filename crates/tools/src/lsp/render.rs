//! Semantic LSP result projection shared by model and enhanced TML views.

use std::fmt::Write as _;

use omp_core::{Str, StrMut};
use serde_json::Value;

/// Stable `SymbolKind` label.
pub fn symbol_kind(kind: u64) -> &'static str {
	const LABELS: [&str; 27] = [
		"unknown",
		"file",
		"module",
		"namespace",
		"package",
		"class",
		"method",
		"property",
		"field",
		"constructor",
		"enum",
		"interface",
		"function",
		"variable",
		"constant",
		"string",
		"number",
		"boolean",
		"array",
		"object",
		"key",
		"null",
		"enum member",
		"struct",
		"event",
		"operator",
		"type parameter",
	];
	LABELS.get(kind as usize).copied().unwrap_or("symbol")
}

/// Bounded structural JSON projection with symbol labels and location lines.
pub fn structured(value: &Value, limit: usize) -> Str {
	let values = value
		.as_array()
		.map_or_else(|| vec![value], |values| values.iter().take(limit).collect());
	let mut output = StrMut::new("");
	for value in values {
		if let Some(name) = value.get("name").and_then(Value::as_str) {
			let kind = value
				.get("kind")
				.and_then(Value::as_u64)
				.map_or("symbol", symbol_kind);
			output.push_str(kind);
			output.push_str(" ");
			output.push_str(name);
			if let Some(line) = value
				.pointer("/location/range/start/line")
				.or_else(|| value.pointer("/range/start/line"))
				.and_then(Value::as_u64)
			{
				output.push_str(" @ line ");
				output.push_str((line + 1).to_string().as_str());
			}
			output.push_str("\n");
		} else if let Some(uri) = value.get("uri").and_then(Value::as_str) {
			output.push_str(uri);
			if let Some(line) = value.pointer("/range/start/line").and_then(Value::as_u64) {
				output.push_str(":");
				output.push_str((line + 1).to_string().as_str());
			}
			output.push_str("\n");
		} else {
			output.push_str(serde_json::to_string(value).unwrap_or_default().as_str());
			output.push_str("\n");
		}
	}
	output.freeze()
}

/// Document-symbol projection: one line per symbol with its kind, name,
/// container, and one-based line range, in server order.
///
/// ```text
/// struct Widget @ lines 3-7
/// method new in Widget @ lines 10-14
/// const MAX @ line 2
/// ```
///
/// The range is the symbol's full extent, so `read path:START-END` fetches it,
/// and `read path:@Container.name` reads it by name. The client does not
/// advertise hierarchical document symbols, so servers answer with flat
/// `SymbolInformation` whose `containerName` carries the nesting; a server
/// that nests `DocumentSymbol.children` anyway is flattened depth first, each
/// child's container being its parent. One result per language server may
/// arrive wrapped in an outer array and is flattened too. The projection never
/// truncates: the central spill gate bounds what the model sees.
pub fn document_symbols(value: &Value) -> Str {
	let mut output = StrMut::new("");
	let mut count = 0_usize;
	let mut visit = |symbol: &Value, container: Option<&str>| {
		count += 1;
		write_document_symbol(&mut output, symbol, container);
	};
	visit_symbols(value, None, &mut visit);
	if count == 0 {
		return Str::new_static("No symbols found");
	}
	output.freeze()
}

fn visit_symbols<'a>(
	value: &'a Value,
	container: Option<&'a str>,
	visit: &mut impl FnMut(&'a Value, Option<&'a str>),
) {
	match value {
		Value::Null => {},
		Value::Array(values) => {
			for value in values {
				visit_symbols(value, container, visit);
			}
		},
		symbol => {
			visit(symbol, container);
			if let Some(children) = symbol.get("children") {
				let parent = symbol.get("name").and_then(Value::as_str).or(container);
				visit_symbols(children, parent, visit);
			}
		},
	}
}

fn write_document_symbol(output: &mut StrMut, symbol: &Value, container: Option<&str>) {
	let Some(name) = symbol.get("name").and_then(Value::as_str) else {
		let _ = writeln!(output, "{}", serde_json::to_string(symbol).unwrap_or_default());
		return;
	};
	let kind = symbol
		.get("kind")
		.and_then(Value::as_u64)
		.map_or("symbol", symbol_kind);
	let container = symbol
		.get("containerName")
		.and_then(Value::as_str)
		.filter(|name| !name.is_empty())
		.or(container);
	let _ = write!(output, "{kind} {name}");
	if let Some(container) = container {
		let _ = write!(output, " in {container}");
	}
	let range = symbol
		.pointer("/location/range")
		.or_else(|| symbol.get("range"));
	if let Some((start, end)) = range.and_then(line_span) {
		if start == end {
			let _ = write!(output, " @ line {start}");
		} else {
			let _ = write!(output, " @ lines {start}-{end}");
		}
	}
	let _ = writeln!(output);
}

/// One-based inclusive `(first, last)` lines of an LSP range.
///
/// A range ending at column zero of a later line stops at the end of the
/// previous one: the end position is exclusive, so a symbol that spans whole
/// lines does not claim the line after it.
fn line_span(range: &Value) -> Option<(u64, u64)> {
	let start = range.pointer("/start/line").and_then(Value::as_u64)?;
	let end = range.pointer("/end/line").and_then(Value::as_u64)?;
	let end_character = range.pointer("/end/character").and_then(Value::as_u64);
	let last = if end > start && end_character == Some(0) {
		end
	} else {
		end + 1
	};
	Some((start + 1, last.max(start + 1)))
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn flat(name: &str, kind: u64, container: Option<&str>, start: u64, end: (u64, u64)) -> Value {
		let mut symbol = json!({
			"name": name,
			"kind": kind,
			"location": {
				"uri": "file:///w/src/lib.rs",
				"range": {
					"start": {"line": start, "character": 0},
					"end": {"line": end.0, "character": end.1},
				},
			},
		});
		if let Some(container) = container {
			symbol["containerName"] = json!(container);
		}
		symbol
	}

	#[test]
	fn flat_symbol_information_shows_kind_container_and_the_read_ready_range() {
		let symbols = json!([
			flat("Widget", 23, None, 2, (6, 1)),
			flat("new", 6, Some("Widget"), 9, (13, 2)),
			flat("MAX", 14, None, 20, (20, 18)),
		]);
		assert_eq!(
			document_symbols(&symbols),
			"struct Widget @ lines 3-7\nmethod new in Widget @ lines 10-14\nconstant MAX @ line 21\n"
		);
	}

	#[test]
	fn a_range_ending_at_column_zero_does_not_claim_the_next_line() {
		let symbols = json!([flat("whole", 12, None, 4, (9, 0)), flat("empty", 12, None, 4, (4, 0))]);
		assert_eq!(
			document_symbols(&symbols),
			"function whole @ lines 5-9\nfunction empty @ line 5\n",
			"an exclusive end at column 0 stops at the previous line, never before the start"
		);
	}

	#[test]
	fn hierarchical_document_symbols_are_flattened_with_their_parent_as_container() {
		let symbols = json!([{
			"name": "Widget", "kind": 23,
			"range": {"start": {"line": 2, "character": 0}, "end": {"line": 14, "character": 1}},
			"selectionRange": {"start": {"line": 2, "character": 11}, "end": {"line": 2, "character": 17}},
			"children": [{
				"name": "new", "kind": 6,
				"range": {"start": {"line": 9, "character": 1}, "end": {"line": 13, "character": 2}},
				"selectionRange": {"start": {"line": 11, "character": 8}, "end": {"line": 11, "character": 11}},
				"children": [{
					"name": "inner", "kind": 13,
					"range": {"start": {"line": 10, "character": 2}, "end": {"line": 10, "character": 20}},
				}],
			}],
		}]);
		assert_eq!(
			document_symbols(&symbols),
			"struct Widget @ lines 3-15\nmethod new in Widget @ lines 10-14\nvariable inner in new @ \
			 line 11\n"
		);
	}

	#[test]
	fn results_from_several_servers_are_flattened_in_order() {
		let symbols =
			json!([[flat("a", 12, None, 0, (1, 1))], null, [flat("b", 12, None, 5, (6, 1))]]);
		assert_eq!(document_symbols(&symbols), "function a @ lines 1-2\nfunction b @ lines 6-7\n");
	}

	#[test]
	fn empty_and_null_results_say_so() {
		assert_eq!(document_symbols(&json!([])), "No symbols found");
		assert_eq!(document_symbols(&Value::Null), "No symbols found");
	}

	#[test]
	fn symbols_without_a_range_or_kind_still_render() {
		let symbols = json!([{"name": "loose"}, {"name": "kinded", "kind": 6, "containerName": ""}]);
		assert_eq!(document_symbols(&symbols), "symbol loose\nmethod kinded\n");
	}

	#[test]
	fn unnamed_entries_fall_back_to_their_json() {
		assert_eq!(document_symbols(&json!([{"state": "ready"}])), "{\"state\":\"ready\"}\n");
	}
}
