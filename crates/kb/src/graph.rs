use std::{
	collections::{BTreeMap, BTreeSet},
	iter,
	path::{Component, Path},
};

use ast_grep_core::tree_sitter::LanguageExt;
use omp_ast::SupportLang;
use protobuf::Message;
use scip::types::{Document, Occurrence, PositionEncoding, occurrence::Typed_range};
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};
use xutf::{Encoding, Utf8, Utf16};

use crate::{
	error::{Context, Error, Result},
	store::Snapshot,
};

mod syntax;

use syntax::is_function;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub(crate) struct Location {
	path:   String,
	line:   usize,
	column: usize,
}

impl Location {
	fn new(path: &str, line: usize, column: usize) -> Self {
		Self { path: path.to_owned(), line: line + 1, column: column + 1 }
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
	#[default]
	Scip,
	Syntactic,
}

impl Resolution {
	fn is_scip(&self) -> bool {
		*self == Self::Scip
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Symbol {
	name:        String,
	definitions: BTreeSet<Location>,
	references:  BTreeSet<Location>,
	#[serde(default, skip_serializing_if = "Resolution::is_scip")]
	resolution:  Resolution,
}

impl Symbol {
	fn name_if_empty(&mut self, name: &str) {
		if self.name.is_empty() {
			self.name = name.to_owned();
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
struct Call {
	caller:   String,
	callee:   String,
	location: Location,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
struct Implementation {
	symbol:   String,
	name:     String,
	location: Location,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Index {
	pub(crate) version:       u32,
	pub snapshot:             Snapshot,
	symbols:                  BTreeMap<String, Symbol>,
	calls:                    BTreeSet<Call>,
	implementations:          BTreeMap<String, BTreeSet<Implementation>>,
	implementation_relations: BTreeMap<String, BTreeSet<String>>,
	reference_relations:      BTreeMap<String, BTreeSet<String>>,
	definition_relations:     BTreeMap<String, BTreeSet<String>>,
}

#[derive(Serialize)]
struct Related<'a> {
	symbol:   &'a str,
	name:     &'a str,
	#[serde(flatten)]
	location: &'a Location,
}

#[derive(Serialize)]
struct Match<'a> {
	symbol:          &'a str,
	name:            &'a str,
	resolution:      Resolution,
	definitions:     BTreeSet<&'a Location>,
	references:      BTreeSet<&'a Location>,
	implementations: Vec<Related<'a>>,
	callers:         Vec<Related<'a>>,
	callees:         Vec<Related<'a>>,
}

#[derive(Serialize)]
pub struct Query<'a> {
	source:      &'static str,
	stale:       bool,
	revision:    &'a str,
	fingerprint: &'a str,
	query:       &'a str,
	miss:        bool,
	results:     Vec<Match<'a>>,
}

#[derive(Clone, Copy)]
struct Span {
	start: (usize, usize),
	end:   (usize, usize),
}

struct At {
	span:       Span,
	symbol:     String,
	definition: bool,
}

fn symbol_id(path: &str, symbol: &str) -> String {
	if symbol.starts_with("local ") {
		format!("{path}::{symbol}")
	} else {
		symbol.to_owned()
	}
}

fn span(occurrence: &Occurrence) -> Result<Span> {
	let (line, col, end_line, end_col) = match &occurrence.typed_range {
		Some(Typed_range::SingleLineRange(r)) => (r.line, r.start_character, r.line, r.end_character),
		Some(Typed_range::MultiLineRange(r)) => {
			(r.start_line, r.start_character, r.end_line, r.end_character)
		},
		Some(_) => return Err(Error::message("unsupported SCIP range encoding")),
		None => match occurrence.range.as_slice() {
			[line, col, end_col] => (*line, *col, *line, *end_col),
			[line, col, end_line, end_col] => (*line, *col, *end_line, *end_col),
			_ => return Err(Error::message("invalid SCIP occurrence range")),
		},
	};
	let start = (usize::try_from(line)?, usize::try_from(col)?);
	let end = (usize::try_from(end_line)?, usize::try_from(end_col)?);
	if end < start {
		return Err(Error::message("invalid SCIP occurrence range order"));
	}
	Ok(Span { start, end })
}

#[derive(Clone, Copy)]
enum Columns {
	Utf8,
	Utf16,
	Utf32,
}

impl Columns {
	fn of(document: &Document) -> Result<Self> {
		match document.position_encoding.enum_value() {
			Ok(
				PositionEncoding::UnspecifiedPositionEncoding
				| PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
			) => Ok(Self::Utf8),
			Ok(PositionEncoding::UTF16CodeUnitOffsetFromLineStart) => Ok(Self::Utf16),
			Ok(PositionEncoding::UTF32CodeUnitOffsetFromLineStart) => Ok(Self::Utf32),
			Err(_) => return Err(Error::message("unsupported SCIP position encoding")),
		}
	}
}

struct Lines<'a> {
	source: &'a [u8],
	starts: Vec<usize>,
}

impl<'a> Lines<'a> {
	fn new(source: &'a [u8]) -> Self {
		let starts = iter::once(0)
			.chain(
				source
					.iter()
					.enumerate()
					.filter(|(_, byte)| **byte == b'\n')
					.map(|(offset, _)| offset + 1),
			)
			.collect();
		Self { source, starts }
	}

	fn line(&self, row: usize) -> Result<&'a [u8]> {
		let start = *self.starts.get(row).context("SCIP line outside document")?;
		let end = self
			.starts
			.get(row + 1)
			.map_or(self.source.len(), |next| next - 1);
		Ok(&self.source[start..end])
	}

	fn byte_point(&self, (row, column): (usize, usize), columns: Columns) -> Result<(usize, usize)> {
		if matches!(columns, Columns::Utf8) {
			return Ok((row, column));
		}
		let line = self.line(row)?;
		let mut rest = line;
		let mut units = 0;
		while units < column && !rest.is_empty() {
			let codepoint = Utf8::decode(&mut rest);
			units += match columns {
				Columns::Utf16 => <Utf16>::encoded_length(codepoint),
				Columns::Utf8 | Columns::Utf32 => 1,
			};
		}
		if units != column {
			return Err(Error::message("SCIP column is outside its line or splits a character"));
		}
		Ok((row, line.len() - rest.len()))
	}

	fn text(&self, span: Span) -> &'a str {
		if span.start.0 != span.end.0 {
			return "";
		}
		self
			.line(span.start.0)
			.ok()
			.and_then(|line| line.get(span.start.1..span.end.1))
			.and_then(|bytes| std::str::from_utf8(bytes).ok())
			.unwrap_or("")
	}
}

fn valid_path(path: &str) -> bool {
	!path.is_empty()
		&& Path::new(path)
			.components()
			.all(|part| matches!(part, Component::Normal(_)))
}

fn token(mut node: Node<'_>) -> Node<'_> {
	loop {
		let field = match node.kind() {
			"field_expression" | "selector_expression" => "field",
			"member_expression" => "property",
			"attribute" => "attribute",
			"scoped_identifier" | "scoped_type_identifier" => "name",
			"generic_function" => "function",
			"generic_type" => "type",
			"reference_type" | "pointer_type" => "type",
			_ => return node,
		};
		let Some(child) = node.child_by_field_name(field) else {
			return node;
		};
		node = child;
	}
}

fn at_node<'a>(
	node: Node<'_>,
	positions: &'a BTreeMap<(usize, usize), Vec<At>>,
	definition: bool,
) -> Option<&'a str> {
	let node = token(node);
	let start = node.start_position();
	let end = node.end_position();
	positions
		.get(&(start.row, start.column))?
		.iter()
		.find(|at| at.definition == definition && at.span.end == (end.row, end.column))
		.map(|at| at.symbol.as_str())
}

struct Syntax<'a> {
	path:      &'a str,
	source:    &'a [u8],
	positions: &'a BTreeMap<(usize, usize), Vec<At>>,
	index:     &'a mut Index,
}

impl Syntax<'_> {
	fn visit(&mut self, node: Node<'_>, caller: Option<&str>) {
		let positions = self.positions;
		let own_caller = if is_function(node.kind()) {
			node
				.child_by_field_name("name")
				.and_then(|name| at_node(name, positions, true))
		} else {
			caller
		};
		if matches!(node.kind(), "call_expression" | "call") {
			if let (Some(caller), Some(function)) = (own_caller, node.child_by_field_name("function"))
			{
				if let Some(callee) = at_node(function, positions, false) {
					let name_node = token(function);
					let name = name_node.utf8_text(self.source).unwrap_or("");
					self
						.index
						.symbols
						.entry(callee.to_owned())
						.or_default()
						.name_if_empty(name);
					let point = name_node.start_position();
					self.index.calls.insert(Call {
						caller:   caller.to_owned(),
						callee:   callee.to_owned(),
						location: Location::new(self.path, point.row, point.column),
					});
				}
			}
		}
		if node.kind() == "impl_item" {
			self.add_implementation(node);
		}
		let mut cursor = node.walk();
		for child in node.named_children(&mut cursor) {
			self.visit(child, own_caller);
		}
	}

	fn add_implementation(&mut self, node: Node<'_>) {
		let (Some(trait_node), Some(type_node)) =
			(node.child_by_field_name("trait"), node.child_by_field_name("type"))
		else {
			return;
		};
		let Some(trait_id) = at_node(trait_node, self.positions, false) else {
			return;
		};
		let type_id = at_node(type_node, self.positions, false).unwrap_or("");
		let Ok(type_name) = type_node.utf8_text(self.source) else {
			return;
		};
		let point = node.start_position();
		self
			.index
			.implementations
			.entry(trait_id.to_owned())
			.or_default()
			.insert(Implementation {
				symbol:   type_id.to_owned(),
				name:     type_name.to_owned(),
				location: Location::new(self.path, point.row, point.column),
			});
		let Some(body) = node.child_by_field_name("body") else {
			return;
		};
		let mut cursor = body.walk();
		for method in body
			.named_children(&mut cursor)
			.filter(|child| child.kind() == "function_item")
		{
			let Some(name) = method.child_by_field_name("name") else {
				continue;
			};
			let Some(method_id) = at_node(name, self.positions, true) else {
				continue;
			};
			let Ok(method_name) = name.utf8_text(self.source) else {
				continue;
			};
			let targets: Vec<_> = self
				.index
				.symbols
				.iter()
				.filter(|(id, symbol)| id.starts_with(trait_id) && symbol.name == method_name)
				.map(|(id, _)| id.clone())
				.collect();
			let point = name.start_position();
			for target in targets {
				self
					.index
					.reference_relations
					.entry(target.clone())
					.or_default()
					.insert(method_id.to_owned());
				self
					.index
					.implementations
					.entry(target)
					.or_default()
					.insert(Implementation {
						symbol:   method_id.to_owned(),
						name:     method_name.to_owned(),
						location: Location::new(self.path, point.row, point.column),
					});
			}
		}
	}
}

impl Index {
	pub(crate) fn read_scip(bytes: &[u8]) -> Result<scip::types::Index> {
		let raw = scip::types::Index::parse_from_bytes(bytes)
			.map_err(|error| Error::foreign("invalid SCIP data", error))?;
		if raw.documents.is_empty()
			|| !raw
				.documents
				.iter()
				.flat_map(|d| &d.occurrences)
				.any(|o| o.symbol_roles & 1 != 0 && !o.symbol.is_empty())
		{
			return Err(Error::message("SCIP index has no definitions"));
		}
		Ok(raw)
	}

	pub(crate) fn decode(root: &Path, raw: &scip::types::Index, snapshot: Snapshot) -> Result<Self> {
		let mut index = Self {
			version: 1,
			snapshot,
			symbols: BTreeMap::new(),
			calls: BTreeSet::new(),
			implementations: BTreeMap::new(),
			implementation_relations: BTreeMap::new(),
			reference_relations: BTreeMap::new(),
			definition_relations: BTreeMap::new(),
		};
		for document in &raw.documents {
			if !valid_path(&document.relative_path) {
				return Err(Error::message(format!(
					"unsafe SCIP document path: {:?}",
					document.relative_path
				)));
			}
			Columns::of(document)?;
			index.add_symbols(document);
		}
		for info in &raw.external_symbols {
			index
				.symbols
				.entry(info.symbol.clone())
				.or_default()
				.name_if_empty(&info.display_name);
		}
		for document in &raw.documents {
			index.add_occurrences(root, document)?;
		}
		Ok(index)
	}

	pub(crate) fn document_paths(raw: &scip::types::Index) -> Result<Vec<String>> {
		raw.documents
			.iter()
			.map(|doc| {
				if !valid_path(&doc.relative_path) {
					return Err(Error::message(format!(
						"unsafe SCIP document path: {:?}",
						doc.relative_path
					)));
				}
				Ok(doc.relative_path.clone())
			})
			.collect()
	}

	pub(crate) fn has_definitions(&self) -> bool {
		self
			.symbols
			.values()
			.any(|symbol| !symbol.definitions.is_empty())
	}

	pub(crate) fn add_wasm_tag(&mut self, wasm: &str, name: &str, rust: &str) {
		let symbol = self.symbols.entry(format!("wasm-tag:{wasm}")).or_default();
		symbol.name = name.to_owned();
		symbol.resolution = Resolution::Syntactic;
		symbol.definitions.insert(Location::new(rust, 0, 0));
	}

	fn add_symbols(&mut self, document: &Document) {
		let path = &document.relative_path;
		for info in &document.symbols {
			if info.symbol.is_empty() {
				continue;
			}
			let id = symbol_id(path, &info.symbol);
			self
				.symbols
				.entry(id.clone())
				.or_default()
				.name_if_empty(&info.display_name);
			for relation in &info.relationships {
				let target = symbol_id(path, &relation.symbol);
				if relation.is_reference {
					self
						.reference_relations
						.entry(target.clone())
						.or_default()
						.insert(id.clone());
				}
				if relation.is_definition {
					self
						.definition_relations
						.entry(id.clone())
						.or_default()
						.insert(target.clone());
				}
				if relation.is_implementation {
					self
						.implementation_relations
						.entry(target)
						.or_default()
						.insert(id.clone());
				}
			}
		}
	}

	fn add_occurrences(&mut self, root: &Path, document: &Document) -> Result<()> {
		let path = &document.relative_path;
		let source =
			std::fs::read(root.join(path)).context(format!("reading SCIP document {path}"))?;
		let lines = Lines::new(&source);
		let columns = Columns::of(document)?;
		let mut positions: BTreeMap<(usize, usize), Vec<At>> = BTreeMap::new();
		for occurrence in &document.occurrences {
			if occurrence.symbol.is_empty() {
				continue;
			}
			let mut span = span(occurrence).context(format!("{path}: invalid occurrence"))?;
			span.start = lines.byte_point(span.start, columns)?;
			span.end = lines.byte_point(span.end, columns)?;
			let id = symbol_id(path, &occurrence.symbol);
			let definition = occurrence.symbol_roles & 1 != 0;
			let location = Location::new(path, span.start.0, span.start.1);
			let symbol = self.symbols.entry(id.clone()).or_default();
			if definition {
				symbol.name_if_empty(lines.text(span));
				symbol.definitions.insert(location);
			} else {
				symbol.references.insert(location);
			}
			positions
				.entry(span.start)
				.or_default()
				.push(At { span, symbol: id, definition });
		}
		if let Some(language) = SupportLang::from_path(Path::new(path)) {
			let mut parser = Parser::new();
			parser
				.set_language(&language.get_ts_language())
				.map_err(|error| Error::foreign("setting parser language", error))?;
			let tree = parser.parse(&source, None).context("cannot parse source")?;
			Syntax { path, source: &source, positions: &positions, index: self }
				.visit(tree.root_node(), None);
		}
		Ok(())
	}

	fn closure<'a>(
		&'a self,
		id: &'a str,
		edges: &'a BTreeMap<String, BTreeSet<String>>,
	) -> BTreeSet<&'a str> {
		let mut seen = BTreeSet::from([id]);
		let mut pending = vec![id];
		while let Some(current) = pending.pop() {
			if let Some(next) = edges.get(current) {
				for target in next {
					if seen.insert(target) {
						pending.push(target);
					}
				}
			}
		}
		seen
	}

	fn related<'a>(&'a self, symbol: &'a str, location: &'a Location) -> Related<'a> {
		Related { symbol, name: self.symbols.get(symbol).map_or("", |s| s.name.as_str()), location }
	}

	pub fn query<'a>(&'a self, needle: &'a str, source: &'static str, stale: bool) -> Query<'a> {
		let results: Vec<_> = self
			.symbols
			.iter()
			.filter(|(id, symbol)| {
				(!symbol.name.is_empty() && symbol.name == needle) || id.as_str() == needle
			})
			.map(|(id, symbol)| {
				let defs = self.closure(id, &self.definition_relations);
				let refs = self.closure(id, &self.reference_relations);
				let definitions = defs
					.iter()
					.filter_map(|id| self.symbols.get(*id))
					.flat_map(|s| &s.definitions)
					.collect();
				let references = refs
					.iter()
					.filter_map(|id| self.symbols.get(*id))
					.flat_map(|s| &s.references)
					.collect();
				let mut implementations: Vec<_> = self
					.implementations
					.get(id)
					.into_iter()
					.flatten()
					.map(|imp| Related {
						symbol:   &imp.symbol,
						name:     &imp.name,
						location: &imp.location,
					})
					.collect();
				for related in self.implementation_relations.get(id).into_iter().flatten() {
					if let Some(info) = self.symbols.get(related) {
						implementations.extend(
							info
								.definitions
								.iter()
								.map(|location| self.related(related, location)),
						);
					}
				}
				let callers = self
					.calls
					.iter()
					.filter(|call| refs.contains(call.callee.as_str()))
					.map(|call| self.related(&call.caller, &call.location))
					.collect();
				let callees = self
					.calls
					.iter()
					.filter(|call| call.caller == *id)
					.map(|call| self.related(&call.callee, &call.location))
					.collect();
				Match {
					symbol: id,
					name: &symbol.name,
					resolution: symbol.resolution,
					definitions,
					references,
					implementations,
					callers,
					callees,
				}
			})
			.collect();
		let source = if source != "live"
			&& !results.is_empty()
			&& results
				.iter()
				.all(|item| item.resolution == Resolution::Syntactic)
		{
			"syntactic"
		} else {
			source
		};
		Query {
			source,
			stale,
			revision: &self.snapshot.revision,
			fingerprint: &self.snapshot.fingerprint,
			query: needle,
			miss: results.is_empty(),
			results,
		}
	}
}
