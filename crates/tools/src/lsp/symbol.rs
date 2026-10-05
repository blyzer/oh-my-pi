//! Symbol addressing: `file` + dotted `symbol` without `line`.
//!
//! The query is resolved against the file's current text with tree-sitter alone
//! (`omp_ast::symbol`): no index, no language server, no extra I/O. One match
//! becomes the declaration's *name* position, which the host hands to the
//! ordinary position path unchanged. Zero or several matches are typed errors:
//! ambiguity is never guessed, and every candidate carries the `line` that
//! retries it.

use std::{
	fmt::{self, Display, Formatter},
	path::Path,
};

use omp_ast::{
	SupportLang,
	symbol::{SourcePoint, SymbolError, SymbolMatch, SymbolQuery, SymbolQueryError, find_symbols},
};
use omp_core::{Str, sf};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One declaration a symbol query matched, kept for an ambiguity report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Candidate {
	/// Declaration kind, as its kebab-case name (`method`, `struct`, ...).
	pub kind:       Str,
	/// Qualified path: enclosing containers then the name, joined by `.`.
	pub path:       Str,
	/// Dotted enclosing modules or namespaces; empty at top level.
	pub module:     Str,
	/// Implemented trait for items of `impl Trait for Type`.
	pub via_trait:  Option<Str>,
	/// First line of the declaration, attributes and doc comments included.
	pub start_line: u32,
	/// Last line of the declaration.
	pub end_line:   u32,
	/// One-based line holding the declaration's own name: the `line` that
	/// retries this candidate.
	pub name_line:  u32,
}

impl Candidate {
	fn new(found: &SymbolMatch, source: &str) -> Self {
		Self {
			kind:       Str::new_static(found.kind.into()),
			path:       found.path.clone(),
			module:     found.module.clone(),
			via_trait:  found.via_trait.clone(),
			start_line: found.start_line,
			end_line:   found.end_line,
			name_line:  found
				.name_point(source)
				.map_or(found.start_line, |point| point.line),
		}
	}
}

/// Why a symbol address did not resolve to exactly one declaration.
///
/// Typed, serializable facts: it is carried by [`super::Fault::Symbol`] into
/// the durable journal, and renders the model-facing text exactly once, through
/// `Display`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SymbolFault {
	/// The query text is not `name(.name)*`.
	#[error(
		"Invalid symbol '{query}': {source}. Use a dotted name such as 'Type.method', or pass \
		 'line' with an identifier."
	)]
	InvalidQuery {
		/// Query text as given.
		query:  Str,
		/// Why the query is malformed.
		#[source]
		source: SymbolQueryError,
	},
	/// The path has no known source language.
	#[error(
		"Cannot resolve symbol '{query}' in '{path}': no known source language for this path. Pass \
		 'line' with an identifier instead."
	)]
	UnknownLanguage {
		/// Query text as given.
		query: Str,
		/// File whose language could not be inferred.
		path:  Str,
	},
	/// The file's language has no declaration table.
	#[error(
		"Cannot resolve symbol '{query}' in '{path}': symbol lookup is not supported for \
		 {language}. Pass 'line' with an identifier instead."
	)]
	UnsupportedLanguage {
		/// Query text as given.
		query:    Str,
		/// File that was searched.
		path:     Str,
		/// Name of the language without a declaration table.
		language: Str,
	},
	/// Tree-sitter could not parse the file.
	#[error(
		"Cannot resolve symbol '{query}' in '{path}': the source could not be parsed. Pass 'line' \
		 with an identifier instead."
	)]
	ParseFailed {
		/// Query text as given.
		query: Str,
		/// File that was searched.
		path:  Str,
	},
	/// No declaration in the file matches the query.
	#[error(
		"No declaration matches symbol '{query}' in '{path}'. Names are exact and case-sensitive; \
		 nest with dots (Type.method). Pass 'line' with an identifier instead."
	)]
	NotFound {
		/// Query text as given.
		query: Str,
		/// File that was searched.
		path:  Str,
	},
	/// More than one declaration matches the query.
	#[error(
		"Symbol '{query}' is ambiguous in '{path}': {} declarations match. Retry with 'line' set to \
		 a candidate's name line (and 'symbol' set to its bare identifier) or a longer dotted query \
		 (Type.method):{}",
		.candidates.len(),
		CandidateLines(.path, .candidates)
	)]
	Ambiguous {
		/// Query text as given.
		query:      Str,
		/// File that was searched.
		path:       Str,
		/// Every match, ordered by start line.
		candidates: Box<[Candidate]>,
	},
	/// The matched declaration's name is not inside the text it was found in.
	#[error(
		"Cannot locate the name of symbol '{query}' in '{path}': the file changed during lookup."
	)]
	Unlocatable {
		/// Query text as given.
		query: Str,
		/// File that was searched.
		path:  Str,
	},
}

// Pin the footprint: this rides inside every symbol-addressed fault.
const _: () = assert!(size_of::<SymbolFault>() <= 80, "SymbolFault must stay compact");

/// One line per ambiguity candidate: kind, qualified path, trait and module,
/// then the range that reads it and the `line` that retries it.
struct CandidateLines<'a>(&'a str, &'a [Candidate]);

impl Display for CandidateLines<'_> {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		for candidate in self.1 {
			write!(f, "\n- {} {}", candidate.kind, candidate.path)?;
			if let Some(via_trait) = &candidate.via_trait {
				write!(f, " via {via_trait}")?;
			}
			if !candidate.module.is_empty() {
				write!(f, " in {}", candidate.module)?;
			}
			write!(
				f,
				": {}:{}-{} (line={})",
				self.0, candidate.start_line, candidate.end_line, candidate.name_line
			)?;
		}
		Ok(())
	}
}

/// Resolve the dotted `query` against `text`, the current contents of the file
/// at `path`, to the position of the one declaration's name.
///
/// `path` supplies the language (by extension) and names the file in errors.
/// The point's column counts bytes; the host converts it to the negotiated LSP
/// position encoding from the line's text.
pub fn resolve(path: &str, text: &str, query: &str) -> Result<SourcePoint, SymbolFault> {
	let parsed = SymbolQuery::parse(query)
		.map_err(|source| SymbolFault::InvalidQuery { query: Str::new(query), source })?;
	let language = SupportLang::from_path(Path::new(path)).ok_or_else(|| {
		SymbolFault::UnknownLanguage { query: Str::new(query), path: Str::new(path) }
	})?;
	let found = find_symbols(text, language, &parsed).map_err(|source| match source {
		SymbolError::UnsupportedLanguage { language } => SymbolFault::UnsupportedLanguage {
			query:    Str::new(query),
			path:     Str::new(path),
			language: sf!("{language}"),
		},
		SymbolError::ParseFailed { .. } | SymbolError::Parse(_) => {
			SymbolFault::ParseFailed { query: Str::new(query), path: Str::new(path) }
		},
	})?;
	match found.as_slice() {
		[] => Err(SymbolFault::NotFound { query: Str::new(query), path: Str::new(path) }),
		[only] => only
			.name_point(text)
			.ok_or_else(|| SymbolFault::Unlocatable { query: Str::new(query), path: Str::new(path) }),
		many => Err(SymbolFault::Ambiguous {
			query:      Str::new(query),
			path:       Str::new(path),
			candidates: many
				.iter()
				.map(|found| Candidate::new(found, text))
				.collect(),
		}),
	}
}

#[cfg(test)]
mod tests {
	use std::error::Error as _;

	use super::{super::Fault, *};

	const SOURCE: &str = "\
use std::fmt;

/// A widget.
#[derive(Debug)]
pub struct Widget {
	size: u32,
}

impl Widget {
	/// Make one.
	#[inline]
	pub fn new(size: u32) -> Self {
		Self { size }
	}
}

impl fmt::Display for Widget {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, \"{}\", self.size)
	}
}

impl fmt::Debug for Gadget {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		Ok(())
	}
}
";

	#[test]
	fn unique_match_is_the_name_not_the_attached_attributes() {
		let point = resolve("src/w.rs", SOURCE, "Widget.new").unwrap();
		assert_eq!(point, SourcePoint { line: 12, column: 8 });
		let field = resolve("src/w.rs", SOURCE, "size").unwrap_err();
		assert!(matches!(field, SymbolFault::NotFound { .. }), "fields are not declarations");
	}

	#[test]
	fn ambiguity_lists_candidates_with_ranges_and_the_line_to_retry_with() {
		let error = resolve("src/w.rs", SOURCE, "fmt").unwrap_err();
		assert_eq!(
			error.to_string(),
			"Symbol 'fmt' is ambiguous in 'src/w.rs': 2 declarations match. Retry with 'line' set to \
			 a candidate's name line (and 'symbol' set to its bare identifier) or a longer dotted \
			 query (Type.method):\n- method Widget.fmt via Display: src/w.rs:18-20 (line=18)\n- \
			 method Gadget.fmt via Debug: src/w.rs:24-26 (line=24)"
		);
		let SymbolFault::Ambiguous { candidates, .. } = &error else {
			panic!("expected ambiguity, got {error:?}");
		};
		assert_eq!(candidates.len(), 2);
	}

	#[test]
	fn the_retry_line_is_the_name_line_even_when_attributes_come_first() {
		let error = resolve("src/w.rs", SOURCE, "Widget").unwrap_err();
		let SymbolFault::Ambiguous { candidates, .. } = &error else {
			panic!("expected ambiguity, got {error:?}");
		};
		let rows = candidates
			.iter()
			.map(|candidate| {
				(candidate.kind.as_str(), candidate.start_line, candidate.end_line, candidate.name_line)
			})
			.collect::<Vec<_>>();
		assert_eq!(rows, [("struct", 3, 7, 5), ("impl", 9, 15, 9), ("impl", 17, 21, 17),]);
		// Retrying the struct by its name line resolves through the existing
		// `line` + identifier path: line 5 holds `Widget` exactly once.
		assert!(
			error
				.to_string()
				.contains("struct Widget: src/w.rs:3-7 (line=5)")
		);
	}

	#[test]
	fn not_found_names_the_query_and_the_path() {
		let error = resolve("src/w.rs", SOURCE, "Widget.missing").unwrap_err();
		assert!(matches!(error, SymbolFault::NotFound { .. }));
		assert_eq!(
			error.to_string(),
			"No declaration matches symbol 'Widget.missing' in 'src/w.rs'. Names are exact and \
			 case-sensitive; nest with dots (Type.method). Pass 'line' with an identifier instead."
		);
	}

	#[test]
	fn malformed_queries_carry_the_typed_query_error() {
		for (query, expected) in [
			("", SymbolQueryError::Empty),
			("a..b", SymbolQueryError::EmptySegment { index: 1 }),
			("a b", SymbolQueryError::WhitespaceInSegment { index: 0 }),
		] {
			let error = resolve("src/w.rs", SOURCE, query).unwrap_err();
			let SymbolFault::InvalidQuery { source, .. } = &error else {
				panic!("expected an invalid query, got {error:?}");
			};
			assert_eq!(*source, expected);
			assert!(error.source().is_some());
		}
		assert_eq!(
			resolve("src/w.rs", SOURCE, "a..b").unwrap_err().to_string(),
			"Invalid symbol 'a..b': symbol query segment 1 is empty. Use a dotted name such as \
			 'Type.method', or pass 'line' with an identifier."
		);
	}

	#[test]
	fn unsupported_and_unknown_languages_are_errors_not_empty_results() {
		let unsupported = resolve("data.json", "{}", "key").unwrap_err();
		assert!(
			matches!(&unsupported, SymbolFault::UnsupportedLanguage { language, .. } if language == "Json")
		);
		assert_eq!(
			unsupported.to_string(),
			"Cannot resolve symbol 'key' in 'data.json': symbol lookup is not supported for Json. \
			 Pass 'line' with an identifier instead."
		);
		let unknown = resolve("notes.zzzz", "x", "key").unwrap_err();
		assert!(matches!(unknown, SymbolFault::UnknownLanguage { .. }));
		assert_eq!(
			unknown.to_string(),
			"Cannot resolve symbol 'key' in 'notes.zzzz': no known source language for this path. \
			 Pass 'line' with an identifier instead."
		);
	}

	#[test]
	fn resolution_serves_every_supported_language_and_dotted_nesting() {
		let cases = [
			("a.ts", "export class Config {\n\tload(): void {}\n}\n", "Config.load", (2, 1)),
			("a.py", "class Config:\n    def load(self):\n        pass\n", "Config.load", (2, 8)),
			("a.go", "package a\n\nfunc (c *Config) Load() {}\n", "Config.Load", (3, 17)),
			("A.java", "class Config {\n\tvoid load() {}\n}\n", "Config.load", (2, 6)),
			("a.js", "function load() {}\n", "load", (1, 9)),
		];
		for (path, text, query, (line, column)) in cases {
			assert_eq!(
				resolve(path, text, query).unwrap(),
				SourcePoint { line, column },
				"{path} {query}"
			);
		}
	}

	#[test]
	fn a_resolved_name_converts_to_the_negotiated_encoding_on_a_wide_line() {
		use omp_proto::lsp::PositionEncoding;

		use super::super::navigation::column_in_encoding;

		let text = "const \u{c9}: u8 = 1; fn lone() {}\n";
		let point = resolve("w.rs", text, "lone").unwrap();
		assert_eq!(point, SourcePoint { line: 1, column: 21 });
		let line = text.lines().next().unwrap();
		let utf16 = column_in_encoding(line, point.column as usize, PositionEncoding::Utf16);
		assert_eq!(utf16, Some(20), "UTF-16 counts the capital E acute once");
		let utf8 = column_in_encoding(line, point.column as usize, PositionEncoding::Utf8);
		assert_eq!(utf8, Some(21));
	}

	#[test]
	fn a_symbol_fault_keeps_its_text_and_round_trips_through_the_journal_shape() {
		for query in ["Widget.missing", "fmt", "Widget", "a..b"] {
			let error = resolve("src/w.rs", SOURCE, query).unwrap_err();
			let expected = error.to_string();
			let fault = Fault::from(error.clone());
			assert!(matches!(&fault, Fault::Symbol(inner) if *inner == error), "{query}");
			assert_eq!(fault.to_string(), expected, "{query}: the fault renders the typed text");
			let json = serde_json::to_value(&fault).unwrap();
			assert_eq!(json["kind"], "symbol", "{query}");
			assert!(json["reason"].is_string(), "{query}: {json}");
			let back: Fault = serde_json::from_value(json).unwrap();
			assert_eq!(back.to_string(), expected, "{query}: {back:?}");
		}
		let json = serde_json::to_value(Fault::from(resolve("src/w.rs", SOURCE, "fmt").unwrap_err()))
			.unwrap();
		assert_eq!(json["reason"], "ambiguous");
		assert_eq!(json["candidates"][0]["kind"], "method");
		assert_eq!(json["candidates"][0]["name_line"], 18);
	}
}
