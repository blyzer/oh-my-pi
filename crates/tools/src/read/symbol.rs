//! Symbol selectors: `path:@Type.method` reads exactly one declaration.
//!
//! The selector is resolved against the text `read` already holds, with
//! tree-sitter alone (`omp_ast::symbol`): no index, no LSP, no extra I/O. One
//! match becomes the equivalent [`ParsedSelector::Lines`], so everything
//! downstream — the numbered projection, the `[path#TAG]` header, the seen
//! ranges `edit` relies on, the spill gate — is the range read's own. Zero or
//! several matches are typed errors: ambiguity is never guessed.

use std::{
	fmt::{self, Display, Formatter},
	path::Path,
};

use omp_ast::{
	SupportLang,
	symbol::{SymbolError, SymbolKind, SymbolQuery, SymbolQueryError, find_symbols},
};
use omp_core::Str;
use thiserror::Error;

use super::{
	Fault,
	selector::{LineRange, ParsedSelector},
};

/// One declaration a symbol selector matched, kept for an ambiguity report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
	/// Declaration kind.
	pub kind:       SymbolKind,
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
}

/// Why a symbol selector did not resolve to exactly one declaration.
#[derive(Debug, Error)]
pub enum SymbolReadError {
	/// The query text is not `name(.name)*`.
	#[error("Invalid symbol selector ':@{query}': {source}")]
	InvalidQuery {
		/// Query text without the `@` sigil.
		query:  Str,
		/// Why the query is malformed.
		#[source]
		source: SymbolQueryError,
	},
	/// The path has no known source language.
	#[error(
		"Cannot resolve symbol ':@{query}' in '{path}': no known source language for this path. Use \
		 a line range instead ('{path}:START-END')."
	)]
	UnknownLanguage {
		/// Query text without the `@` sigil.
		query: Str,
		/// File whose language could not be inferred.
		path:  Str,
	},
	/// Tree-sitter lookup is unavailable for the file's language or failed.
	#[error(
		"Cannot resolve symbol ':@{query}' in '{path}': {source}. Use a line range instead \
		 ('{path}:START-END')."
	)]
	Lookup {
		/// Query text without the `@` sigil.
		query:  Str,
		/// File that was searched.
		path:   Str,
		/// Why the lookup could not run.
		#[source]
		source: SymbolError,
	},
	/// No declaration in the file matches the query.
	#[error(
		"No declaration matches symbol ':@{query}' in '{path}'. Names are exact and case-sensitive; \
		 nest with dots (Type.method). Use a line range instead ('{path}:START-END')."
	)]
	NotFound {
		/// Query text without the `@` sigil.
		query: Str,
		/// File that was searched.
		path:  Str,
	},
	/// More than one declaration matches the query.
	#[error(
		"Symbol ':@{query}' is ambiguous in '{path}': {} declarations match. Retry with a line \
		 range ('{path}:START-END') or a longer dotted query (Type.method):{}",
		.candidates.len(),
		CandidateLines(.path, .candidates)
	)]
	Ambiguous {
		/// Query text without the `@` sigil.
		query:      Str,
		/// File that was searched.
		path:       Str,
		/// Every match, ordered by start line.
		candidates: Box<[Candidate]>,
	},
}

/// A symbol read fails as an invalid selector; the typed error renders exactly
/// once, here.
impl From<SymbolReadError> for Fault {
	fn from(error: SymbolReadError) -> Self {
		Self::Invalid { message: Str::new(error.to_string()) }
	}
}

// Pin the footprint: this is the error of every symbol read.
const _: () = assert!(size_of::<SymbolReadError>() <= 112, "SymbolReadError must stay compact");

/// One line per ambiguity candidate, each carrying the range selector that
/// reads it.
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
			write!(f, ": {}:{}-{}", self.0, candidate.start_line, candidate.end_line)?;
		}
		Ok(())
	}
}

/// Resolve `:@query` against `text`, the contents of the file at `path`.
///
/// `path` supplies the language (by extension) and names the file in errors.
/// On a unique match, returns the equivalent line selector; `raw` carries the
/// verbatim flag of a `:raw` combination.
pub fn resolve(
	path: &str,
	text: &str,
	query: &str,
	raw: bool,
) -> Result<ParsedSelector, SymbolReadError> {
	let parsed = SymbolQuery::parse(query)
		.map_err(|source| SymbolReadError::InvalidQuery { query: Str::new(query), source })?;
	let language = SupportLang::from_path(Path::new(path)).ok_or_else(|| {
		SymbolReadError::UnknownLanguage { query: Str::new(query), path: Str::new(path) }
	})?;
	let found = find_symbols(text, language, &parsed).map_err(|source| SymbolReadError::Lookup {
		query: Str::new(query),
		path: Str::new(path),
		source,
	})?;
	match found.as_slice() {
		[] => Err(SymbolReadError::NotFound { query: Str::new(query), path: Str::new(path) }),
		[only] => Ok(ParsedSelector::Lines {
			ranges: Box::from([LineRange {
				start_line: u64::from(only.start_line),
				end_line:   Some(u64::from(only.end_line)),
			}]),
			raw,
		}),
		many => Err(SymbolReadError::Ambiguous {
			query:      Str::new(query),
			path:       Str::new(path),
			candidates: many
				.iter()
				.map(|found| Candidate {
					kind:       found.kind,
					path:       found.path.clone(),
					module:     found.module.clone(),
					via_trait:  found.via_trait.clone(),
					start_line: found.start_line,
					end_line:   found.end_line,
				})
				.collect(),
		}),
	}
}

#[cfg(test)]
mod tests {
	use std::error::Error as _;

	use super::*;

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

	fn lines(selector: &ParsedSelector) -> (u64, Option<u64>, bool) {
		let ParsedSelector::Lines { ranges, raw } = selector else {
			panic!("expected a line selector, got {selector:?}");
		};
		let [range] = ranges.as_ref() else {
			panic!("expected one range, got {ranges:?}");
		};
		(range.start_line, range.end_line, *raw)
	}

	#[test]
	fn unique_match_becomes_its_line_range_with_attributes_and_docs() {
		let resolved = resolve("src/w.rs", SOURCE, "Widget.new", false).unwrap();
		assert_eq!(lines(&resolved), (10, Some(14), false));
		let resolved = resolve("src/w.rs", SOURCE, "Widget.new", true).unwrap();
		assert_eq!(lines(&resolved), (10, Some(14), true));
	}

	#[test]
	fn bare_type_name_is_ambiguous_across_the_struct_and_its_impls() {
		let error = resolve("src/w.rs", SOURCE, "Widget", false).unwrap_err();
		let SymbolReadError::Ambiguous { candidates, .. } = &error else {
			panic!("expected ambiguity, got {error:?}");
		};
		let kinds = candidates
			.iter()
			.map(|candidate| (candidate.kind, candidate.start_line, candidate.end_line))
			.collect::<Vec<_>>();
		assert_eq!(kinds, [
			(SymbolKind::Struct, 3, 7),
			(SymbolKind::Impl, 9, 15),
			(SymbolKind::Impl, 17, 21)
		]);
	}

	#[test]
	fn ambiguity_lists_candidates_with_via_trait_and_a_range_to_retry_with() {
		let error = resolve("src/w.rs", SOURCE, "fmt", false).unwrap_err();
		assert_eq!(
			error.to_string(),
			"Symbol ':@fmt' is ambiguous in 'src/w.rs': 2 declarations match. Retry with a line \
			 range ('src/w.rs:START-END') or a longer dotted query (Type.method):\n- method \
			 Widget.fmt via Display: src/w.rs:18-20\n- method Gadget.fmt via Debug: src/w.rs:24-26"
		);
	}

	#[test]
	fn not_found_names_the_query_and_the_path() {
		let error = resolve("src/w.rs", SOURCE, "Widget.missing", false).unwrap_err();
		assert!(matches!(error, SymbolReadError::NotFound { .. }));
		assert_eq!(
			error.to_string(),
			"No declaration matches symbol ':@Widget.missing' in 'src/w.rs'. Names are exact and \
			 case-sensitive; nest with dots (Type.method). Use a line range instead \
			 ('src/w.rs:START-END')."
		);
	}

	#[test]
	fn malformed_queries_carry_the_typed_query_error() {
		for (query, expected) in [
			("", SymbolQueryError::Empty),
			("a..b", SymbolQueryError::EmptySegment { index: 1 }),
			("a b", SymbolQueryError::WhitespaceInSegment { index: 0 }),
		] {
			let error = resolve("src/w.rs", SOURCE, query, false).unwrap_err();
			let SymbolReadError::InvalidQuery { source, .. } = &error else {
				panic!("expected an invalid query, got {error:?}");
			};
			assert_eq!(*source, expected);
			assert!(error.source().is_some());
		}
		assert_eq!(
			resolve("src/w.rs", SOURCE, "", false)
				.unwrap_err()
				.to_string(),
			"Invalid symbol selector ':@': symbol query is empty"
		);
	}

	#[test]
	fn unsupported_and_unknown_languages_are_errors_not_empty_results() {
		let unsupported = resolve("data.json", "{}", "key", false).unwrap_err();
		assert!(matches!(&unsupported, SymbolReadError::Lookup {
			source: SymbolError::UnsupportedLanguage { .. },
			..
		}));
		assert_eq!(
			unsupported.to_string(),
			"Cannot resolve symbol ':@key' in 'data.json': symbol lookup is not supported for Json. \
			 Use a line range instead ('data.json:START-END')."
		);
		let unknown = resolve("notes.zzzz", "x", "key", false).unwrap_err();
		assert!(matches!(unknown, SymbolReadError::UnknownLanguage { .. }));
	}
}
