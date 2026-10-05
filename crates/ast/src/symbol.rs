//! Name-based declaration lookup.
//!
//! [`find_symbols`] answers "where is `Type.method` declared in this source?"
//! with tree-sitter alone: no index, no LSP, no I/O. It returns every
//! declaration whose qualified path ends with the query, with the 1-based
//! inclusive line span of the *whole* item (leading attributes, decorators and
//! outer doc comments included), so a caller can slice exactly that text.
//!
//! # Selector grammar
//!
//! A [`SymbolQuery`] is `segment(.segment)*`. A bare `name` matches any
//! declaration called `name`; `Type.method` matches a declaration `method`
//! nested directly inside `Type`. Matching is exact and case-sensitive.
//!
//! # Qualified paths
//!
//! Every declaration has a *qualified path*: the names of the enclosing
//! containers (classes, structs-as-impl-targets, traits, interfaces, ...)
//! followed by its own name, joined by `.`. Modules and namespaces are **not**
//! part of that path (`mod m { impl S { fn f() {} } }` is `S.f`), and are
//! reported separately in [`SymbolMatch::module`]. A query may nevertheless
//! start at any enclosing declaration, modules included: `m.S.f` and `S.f` and
//! `f` all match it. The query must be a contiguous *suffix* of the chain;
//! `m.f` does not match `m.S.f`.
//!
//! Rust `impl Type { .. }` and `impl Trait for Type { .. }` both make `Type`
//! the container. Methods of a trait impl carry the implemented trait's last
//! path segment in [`SymbolMatch::via_trait`], so two impls on one type that
//! define the same method yield two candidates. The trait name is not a path
//! segment of the impl's items; the trait's *own* declaration is `Trait.m`.
//!
//! # Scope
//!
//! Function bodies are never entered: nested functions and closures are not
//! addressable. Macro-generated items do not exist at the syntax level and are
//! out of scope. Source with syntax errors is searched best-effort on whatever
//! tree tree-sitter recovered.
//!
//! # Languages
//!
//! Rust; TypeScript, TSX and JavaScript; Python; Go; Java. Any other
//! [`SupportLang`] fails with [`SymbolError::UnsupportedLanguage`] rather than
//! returning an empty result.

use std::{fmt::Write as _, iter, ops::Range};

use omp_core::{Str, StrMut};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use strum::{Display, EnumString, IntoStaticStr};
use thiserror::Error;
use tree_sitter::Node;

use crate::{
	AstError,
	language::SupportLang,
	parse_cache::parse_cached,
	summary::{node_content_end_line, node_start_line},
};

/// Why a selector string is not a valid [`SymbolQuery`].
///
/// Serializable so a tool fault can carry it as a typed fact rather than text.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolQueryError {
	/// The selector is the empty string.
	#[error("symbol query is empty")]
	Empty,
	/// A `.`-separated segment has no characters (`a..b`, `.a`, `a.`).
	#[error("symbol query segment {index} is empty")]
	EmptySegment {
		/// 0-based position of the offending segment.
		index: usize,
	},
	/// A segment contains whitespace, which no identifier does.
	#[error("symbol query segment {index} contains whitespace")]
	WhitespaceInSegment {
		/// 0-based position of the offending segment.
		index: usize,
	},
}

/// Why [`find_symbols`] could not search a source.
#[derive(Debug, Error)]
pub enum SymbolError {
	/// Symbol lookup has no declaration table for this language.
	#[error("symbol lookup is not supported for {language}")]
	UnsupportedLanguage {
		/// The language that has no declaration table.
		language: SupportLang,
	},
	/// The grammar loaded but tree-sitter produced no tree.
	#[error("tree-sitter produced no tree for {language} source")]
	ParseFailed {
		/// The language being parsed.
		language: SupportLang,
	},
	/// The grammar could not be loaded.
	#[error("failed to parse source for symbol lookup")]
	Parse(#[from] AstError),
}

/// A parsed selector: one or more non-empty `.`-separated name segments,
/// borrowed from the selector text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolQuery<'a> {
	segments: SmallVec<&'a str, 4>,
}

impl<'a> SymbolQuery<'a> {
	/// Split `selector` on `.` and validate every segment.
	///
	/// The selector is the bare path: callers strip any `@` sigil and file
	/// prefix first.
	pub fn parse(selector: &'a str) -> Result<Self, SymbolQueryError> {
		if selector.is_empty() {
			return Err(SymbolQueryError::Empty);
		}
		let mut segments = SmallVec::new();
		for (index, segment) in selector.split('.').enumerate() {
			if segment.is_empty() {
				return Err(SymbolQueryError::EmptySegment { index });
			}
			if segment.chars().any(char::is_whitespace) {
				return Err(SymbolQueryError::WhitespaceInSegment { index });
			}
			segments.push(segment);
		}
		Ok(Self { segments })
	}

	/// The path segments, outermost first. Never empty.
	pub fn segments(&self) -> &[&'a str] {
		&self.segments
	}

	/// The final segment: the declaration name being looked up.
	pub fn name(&self) -> &'a str {
		self.segments.last().copied().unwrap_or_default()
	}
}

/// What a matched declaration is.
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, Hash, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum SymbolKind {
	/// A free function (also a JS/TS function-valued `const`).
	Function,
	/// A function inside a type-like container: impl/trait item, class or
	/// interface member, Go receiver method, Java method or constructor.
	Method,
	/// A class.
	Class,
	/// A struct (Rust, Go).
	Struct,
	/// An enum.
	Enum,
	/// A Rust union.
	Union,
	/// A Rust trait.
	Trait,
	/// An interface (TypeScript, Java, Go).
	Interface,
	/// A Rust `impl` block, named by its self type.
	Impl,
	/// A module or namespace.
	Module,
	/// A constant or associated constant.
	Const,
	/// A Rust `static`.
	Static,
	/// A type alias or associated type.
	TypeAlias,
	/// A non-constant variable (`let`/`var`, Go `var`).
	Variable,
}

/// One declaration found by [`find_symbols`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolMatch {
	/// Declaration kind.
	pub kind:       SymbolKind,
	/// Qualified path: enclosing containers then the name, joined by `.`.
	/// Modules are excluded; see [`Self::module`].
	pub path:       Str,
	/// Dotted names of the enclosing modules/namespaces; empty at top level.
	pub module:     Str,
	/// For items inside `impl Trait for Type`, the trait's last path segment
	/// (`fmt::Display` gives `Display`); also set on the impl block itself.
	pub via_trait:  Option<Str>,
	/// 1-based first line, including leading attributes, decorators and outer
	/// doc comments.
	pub start_line: u32,
	/// 1-based last line holding content of the declaration.
	pub end_line:   u32,
	/// Byte offset of the first byte of [`Self::start_line`]'s attached text.
	pub start_byte: usize,
	/// Byte offset one past the declaration's last content byte.
	pub end_byte:   usize,
	/// Byte offset of the first byte of the declaration's own name in the
	/// searched source; [`Self::name_point`] turns it into a line and column.
	pub name_byte:  usize,
	name_start:     u32,
}

/// A position in source text: 1-based line, 0-based byte column.
///
/// The column counts bytes from the start of the line, the unit tree-sitter
/// reports. Callers needing UTF-16 or UTF-32 columns convert from the line's
/// text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePoint {
	/// 1-based line.
	pub line:   u32,
	/// 0-based byte offset from the start of [`Self::line`].
	pub column: u32,
}

// Pin the footprint: matches are cloned into candidate lists and diagnostics.
// 112 bytes before `name_byte` (the declaration name's source offset, one word
// that no field can absorb); a regression past 120 fails the build.
const _: () = assert!(size_of::<SymbolMatch>() <= 120, "SymbolMatch must stay compact");

impl SymbolMatch {
	/// The declaration's own name (last path segment).
	pub fn name(&self) -> &str {
		&self.path.as_str()[self.name_start as usize..]
	}

	/// The qualified path of the enclosing container, empty at top level.
	pub fn container(&self) -> &str {
		self.path.as_str()[..self.name_start as usize].trim_end_matches('.')
	}

	/// The attached source text's byte range.
	pub const fn byte_range(&self) -> Range<usize> {
		self.start_byte..self.end_byte
	}

	/// Where the declaration's own name starts in `source`.
	///
	/// `source` must be the text this match was found in. A line is ended by
	/// `\n` only, so `\r\n` sources keep their `\r` at the end of the previous
	/// line and never inside a column. `None` when `source` is shorter than
	/// [`Self::name_byte`] or the offset is not a character boundary, which
	/// means it is not the searched text.
	pub fn name_point(&self, source: &str) -> Option<SourcePoint> {
		let before = source.get(..self.name_byte)?;
		let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
		let newlines = before.bytes().filter(|byte| *byte == b'\n').count();
		Some(SourcePoint {
			line:   u32::try_from(newlines + 1).ok()?,
			column: u32::try_from(before.len() - line_start).ok()?,
		})
	}
}

/// Find every declaration in `source` whose qualified path ends with `query`.
///
/// ```
/// use omp_ast::{
/// 	SupportLang,
/// 	symbol::{SymbolQuery, find_symbols},
/// };
///
/// let source = "struct S;\n\nimpl S {\n\t/// Go.\n\tfn run(&self) {}\n}\n";
/// let query = SymbolQuery::parse("S.run")?;
/// let found = find_symbols(source, SupportLang::Rust, &query)?;
/// assert_eq!((found[0].start_line, found[0].end_line), (4, 5));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Results are sorted by start line. An empty result means "no such
/// declaration"; an unsupported `language` is an error, never an empty result.
/// Ambiguity is not resolved: all candidates are returned.
pub fn find_symbols(
	source: &str,
	language: SupportLang,
	query: &SymbolQuery<'_>,
) -> Result<SmallVec<SymbolMatch, 4>, SymbolError> {
	let family = Family::of(language).ok_or(SymbolError::UnsupportedLanguage { language })?;
	let tree = parse_cached(source, language)?.ok_or(SymbolError::ParseFailed { language })?;
	let mut walker = Walker { source, family, query, scopes: SmallVec::new(), out: SmallVec::new() };
	walker.visit_children(tree.root_node());
	let mut out = walker.out;
	out.sort_unstable_by_key(|found| (found.start_line, found.start_byte, found.end_byte));
	Ok(out)
}

/// Grammar family sharing one declaration table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
	Rust,
	Script,
	Python,
	Go,
	Java,
}

impl Family {
	const fn of(language: SupportLang) -> Option<Self> {
		match language {
			SupportLang::Rust => Some(Self::Rust),
			SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => Some(Self::Script),
			SupportLang::Python => Some(Self::Python),
			SupportLang::Go => Some(Self::Go),
			SupportLang::Java => Some(Self::Java),
			_ => None,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
	/// Contributes to the dotted path.
	Type,
	/// Matchable by queries but kept out of the path.
	Module,
}

/// An enclosing declaration on the walk stack.
struct Scope<'a> {
	name:      &'a str,
	kind:      ScopeKind,
	via_trait: Option<&'a str>,
}

/// A declaration found by a table, before matching.
struct Decl<'a> {
	kind:      SymbolKind,
	name:      &'a str,
	/// Node whose span is the item (e.g. the `export` wrapper, a decorated
	/// definition); attached attributes/comments are found from its siblings.
	outer:     Node<'a>,
	/// Container body to search for nested declarations, and what it scopes.
	nested:    Option<(Node<'a>, ScopeKind)>,
	via_trait: Option<&'a str>,
	/// Go receiver type, acting as the container.
	receiver:  Option<&'a str>,
}

impl<'a> Decl<'a> {
	const fn new(kind: SymbolKind, name: &'a str, outer: Node<'a>) -> Self {
		Self { kind, name, outer, nested: None, via_trait: None, receiver: None }
	}
}

struct Walker<'a, 'q> {
	source: &'a str,
	family: Family,
	query:  &'q SymbolQuery<'q>,
	scopes: SmallVec<Scope<'a>, 8>,
	out:    SmallVec<SymbolMatch, 4>,
}

impl<'a> Walker<'a, '_> {
	fn text(&self, node: Node<'_>) -> &'a str {
		&self.source[node.byte_range()]
	}

	/// Byte offset of `name` in the searched source.
	///
	/// Every declaration name is a slice of the source text (`text` of a
	/// node), so its address locates it without carrying a node per name.
	fn offset_of(&self, name: &str) -> usize {
		let source = self.source.as_bytes().as_ptr_range();
		let at = name.as_ptr();
		debug_assert!(
			source.contains(&at) && name.len() <= source.end.addr() - at.addr(),
			"declaration names are slices of the searched source"
		);
		at.addr().saturating_sub(source.start.addr())
	}

	fn field_text(&self, node: Node<'_>, field: &str) -> Option<&'a str> {
		node.child_by_field_name(field).map(|n| self.text(n))
	}

	fn in_type_scope(&self) -> bool {
		self
			.scopes
			.last()
			.is_some_and(|scope| scope.kind == ScopeKind::Type)
	}

	fn function_kind(&self) -> SymbolKind {
		if self.in_type_scope() {
			SymbolKind::Method
		} else {
			SymbolKind::Function
		}
	}

	fn visit_children(&mut self, node: Node<'a>) {
		let mut cursor = node.walk();
		for child in node.named_children(&mut cursor) {
			self.visit(child, child);
		}
	}

	fn visit(&mut self, node: Node<'a>, outer: Node<'a>) {
		match self.family {
			Family::Rust => self.visit_rust(node),
			Family::Script => self.visit_script(node, outer),
			Family::Python => self.visit_python(node, outer),
			Family::Go => self.visit_go(node),
			Family::Java => self.visit_java(node),
		}
	}

	/// Match `decl`, record it, and search its body for nested declarations.
	fn declare(&mut self, decl: Decl<'a>) {
		if decl.name.is_empty() || decl.name == "_" {
			return;
		}
		if decl.name == self.query.name() && self.chain_matches(&decl) {
			self.record(&decl);
		}
		if let Some((body, kind)) = decl.nested {
			self
				.scopes
				.push(Scope { name: decl.name, kind, via_trait: decl.via_trait });
			self.visit_children(body);
			self.scopes.pop();
		}
	}

	/// Does the query end the full chain (modules included) or the path chain?
	fn chain_matches(&self, decl: &Decl<'a>) -> bool {
		let own = decl.receiver.into_iter().chain(iter::once(decl.name));
		let full = self
			.scopes
			.iter()
			.map(|scope| scope.name)
			.chain(own.clone());
		let path = self
			.scopes
			.iter()
			.filter(|scope| scope.kind == ScopeKind::Type)
			.map(|scope| scope.name)
			.chain(own);
		ends_with_segments(full, self.query.segments())
			|| ends_with_segments(path, self.query.segments())
	}

	fn record(&mut self, decl: &Decl<'a>) {
		let first = self.attach_start(decl.outer);
		let mut path = StrMut::default();
		let mut module = StrMut::default();
		for scope in &self.scopes {
			let (sink, dot) = match scope.kind {
				ScopeKind::Type => (&mut path, '.'),
				ScopeKind::Module => (&mut module, '.'),
			};
			let _ = sink.write_str(scope.name);
			let _ = sink.write_char(dot);
		}
		if let Some(receiver) = decl.receiver {
			let _ = path.write_str(receiver);
			let _ = path.write_char('.');
		}
		let name_start = path.len();
		let _ = path.write_str(decl.name);
		module.truncate(module.len().saturating_sub(1));

		let via_trait = decl.via_trait.or_else(|| {
			self
				.scopes
				.last()
				.filter(|scope| scope.kind == ScopeKind::Type)
				.and_then(|scope| scope.via_trait)
		});
		let end_byte = self.source[..decl.outer.end_byte()]
			.trim_end_matches(['\n', '\r'])
			.len();
		let name_byte = self.offset_of(decl.name);
		self.out.push(SymbolMatch {
			kind: decl.kind,
			path: path.freeze(),
			module: module.freeze(),
			via_trait: via_trait.map(Str::new),
			start_line: node_start_line(first),
			end_line: node_content_end_line(decl.outer),
			start_byte: first.start_byte(),
			end_byte,
			name_byte,
			name_start: name_start as u32,
		});
	}

	/// Earliest directly-preceding sibling that belongs to the item: its
	/// attributes and outer doc comments, with no blank line between.
	fn attach_start(&self, outer: Node<'a>) -> Node<'a> {
		let mut first = outer;
		while let Some(prev) = first.prev_sibling() {
			if !self.is_attached(prev, first) {
				break;
			}
			first = prev;
		}
		first
	}

	fn is_attached(&self, prev: Node<'a>, next: Node<'a>) -> bool {
		let attached = match (self.family, prev.kind()) {
			(Family::Rust, "attribute_item") | (Family::Go, "comment") => true,
			(Family::Rust, "line_comment" | "block_comment") => {
				prev.child_by_field_name("outer").is_some()
			},
			(Family::Script, "comment") | (Family::Java, "block_comment") => {
				let text = self.text(prev);
				text.starts_with("/**") && text != "/**/"
			},
			_ => false,
		};
		attached
			&& node_content_end_line(prev) + 1 >= node_start_line(next)
			// A comment trailing the previous item's last line is not ours.
			&& prev
				.prev_sibling()
				.is_none_or(|before| node_content_end_line(before) < node_start_line(prev))
	}

	fn visit_rust(&mut self, node: Node<'a>) {
		let kind = match node.kind() {
			"function_item" | "function_signature_item" => self.function_kind(),
			"struct_item" => SymbolKind::Struct,
			"enum_item" => SymbolKind::Enum,
			"union_item" => SymbolKind::Union,
			"trait_item" => SymbolKind::Trait,
			"mod_item" => SymbolKind::Module,
			"const_item" => SymbolKind::Const,
			"static_item" => SymbolKind::Static,
			"type_item" | "associated_type" => SymbolKind::TypeAlias,
			"impl_item" => return self.visit_rust_impl(node),
			_ => return,
		};
		let Some(name) = self.field_text(node, "name") else {
			return;
		};
		let mut decl = Decl::new(kind, name, node);
		decl.nested = match kind {
			SymbolKind::Trait => node
				.child_by_field_name("body")
				.map(|body| (body, ScopeKind::Type)),
			SymbolKind::Module => node
				.child_by_field_name("body")
				.map(|body| (body, ScopeKind::Module)),
			_ => None,
		};
		self.declare(decl);
	}

	fn visit_rust_impl(&mut self, node: Node<'a>) {
		let Some(self_type) = node.child_by_field_name("type") else {
			return;
		};
		let mut decl = Decl::new(SymbolKind::Impl, self.rust_type_name(self_type), node);
		decl.via_trait = node
			.child_by_field_name("trait")
			.map(|node| self.rust_type_name(node));
		decl.nested = node
			.child_by_field_name("body")
			.map(|body| (body, ScopeKind::Type));
		self.declare(decl);
	}

	/// Last identifier of a Rust type path, without generics, references or
	/// qualification; the type's source text when it has no single name.
	fn rust_type_name(&self, mut node: Node<'a>) -> &'a str {
		loop {
			let next = match node.kind() {
				"generic_type" | "reference_type" | "pointer_type" => node.child_by_field_name("type"),
				"scoped_type_identifier" => node.child_by_field_name("name"),
				_ => None,
			};
			match next {
				Some(next) => node = next,
				None => return self.text(node),
			}
		}
	}

	fn visit_script(&mut self, node: Node<'a>, outer: Node<'a>) {
		let kind = match node.kind() {
			"export_statement" => {
				if let Some(inner) = node.child_by_field_name("declaration") {
					self.visit_script(inner, node);
				}
				return;
			},
			"ambient_declaration" => {
				let mut cursor = node.walk();
				for inner in node.named_children(&mut cursor) {
					self.visit_script(inner, node);
				}
				return;
			},
			// `namespace N {}` parses as an expression statement wrapping the module.
			"expression_statement" => {
				if let Some(inner) = node
					.named_child(0)
					.filter(|inner| matches!(inner.kind(), "internal_module" | "module"))
				{
					self.visit_script(inner, node);
				}
				return;
			},
			"lexical_declaration" | "variable_declaration" => {
				return self.visit_script_variables(node, outer);
			},
			"function_declaration" | "generator_function_declaration" | "function_signature" => {
				SymbolKind::Function
			},
			"class_declaration" | "abstract_class_declaration" => SymbolKind::Class,
			"interface_declaration" => SymbolKind::Interface,
			"enum_declaration" => SymbolKind::Enum,
			"type_alias_declaration" => SymbolKind::TypeAlias,
			"internal_module" | "module" => SymbolKind::Module,
			"method_definition" | "method_signature" | "abstract_method_signature" => {
				SymbolKind::Method
			},
			"public_field_definition" | "field_definition" => {
				let is_function = node
					.child_by_field_name("value")
					.is_some_and(|value| is_script_function(value.kind()));
				if !is_function {
					return;
				}
				SymbolKind::Method
			},
			_ => return,
		};
		let name_field = if node.kind() == "field_definition" {
			"property"
		} else {
			"name"
		};
		let Some(name_node) = node.child_by_field_name(name_field) else {
			return;
		};
		if matches!(kind, SymbolKind::Method)
			&& !matches!(name_node.kind(), "property_identifier" | "private_property_identifier")
		{
			return;
		}
		let mut decl = Decl::new(kind, self.text(name_node), outer);
		decl.nested = match kind {
			SymbolKind::Class | SymbolKind::Interface => node
				.child_by_field_name("body")
				.map(|body| (body, ScopeKind::Type)),
			SymbolKind::Module => node
				.child_by_field_name("body")
				.map(|body| (body, ScopeKind::Module)),
			_ => None,
		};
		self.declare(decl);
	}

	fn visit_script_variables(&mut self, node: Node<'a>, outer: Node<'a>) {
		let is_const = node
			.child_by_field_name("kind")
			.is_some_and(|kind| self.text(kind) == "const");
		let mut cursor = node.walk();
		for declarator in node
			.named_children(&mut cursor)
			.filter(|child| child.kind() == "variable_declarator")
		{
			let Some(name) = declarator
				.child_by_field_name("name")
				.filter(|name| name.kind() == "identifier")
			else {
				continue;
			};
			let is_function = declarator
				.child_by_field_name("value")
				.is_some_and(|value| is_script_function(value.kind()));
			let kind = match (is_function, is_const) {
				(true, _) => SymbolKind::Function,
				(false, true) => SymbolKind::Const,
				(false, false) => SymbolKind::Variable,
			};
			self.declare(Decl::new(kind, self.text(name), outer));
		}
	}

	fn visit_python(&mut self, node: Node<'a>, outer: Node<'a>) {
		let kind = match node.kind() {
			"decorated_definition" => {
				if let Some(inner) = node.child_by_field_name("definition") {
					self.visit_python(inner, node);
				}
				return;
			},
			"if_statement" | "elif_clause" | "else_clause" | "try_statement" | "except_clause"
			| "finally_clause" | "with_statement" | "block" => {
				return self.visit_children(node);
			},
			"function_definition" => self.function_kind(),
			"class_definition" => SymbolKind::Class,
			_ => return,
		};
		let Some(name) = self.field_text(node, "name") else {
			return;
		};
		let mut decl = Decl::new(kind, name, outer);
		if kind == SymbolKind::Class {
			decl.nested = node
				.child_by_field_name("body")
				.map(|body| (body, ScopeKind::Type));
		}
		self.declare(decl);
	}

	fn visit_go(&mut self, node: Node<'a>) {
		match node.kind() {
			"function_declaration" => {
				if let Some(name) = self.field_text(node, "name") {
					self.declare(Decl::new(SymbolKind::Function, name, node));
				}
			},
			"method_declaration" => {
				let Some(name) = self.field_text(node, "name") else {
					return;
				};
				let mut decl = Decl::new(SymbolKind::Method, name, node);
				decl.receiver = self.go_receiver(node);
				self.declare(decl);
			},
			"type_declaration" => self.visit_go_specs(node, &["type_spec", "type_alias"]),
			"const_declaration" => self.visit_go_specs(node, &["const_spec"]),
			"var_declaration" => self.visit_go_specs(node, &["var_spec"]),
			_ => {},
		}
	}

	/// Declare each spec of a `type`/`const`/`var` declaration. A lone spec
	/// owns the whole declaration (keyword included); grouped specs own only
	/// their own span.
	fn visit_go_specs(&mut self, node: Node<'a>, spec_kinds: &[&str]) {
		let mut specs: SmallVec<Node<'a>, 4> = SmallVec::new();
		let mut cursor = node.walk();
		for child in node.named_children(&mut cursor) {
			if spec_kinds.contains(&child.kind()) {
				specs.push(child);
			} else if child.kind().ends_with("_list") {
				let mut inner = child.walk();
				specs.extend(
					child
						.named_children(&mut inner)
						.filter(|spec| spec_kinds.contains(&spec.kind())),
				);
			}
		}
		let single = specs.len() == 1;
		for spec in specs {
			let outer = if single { node } else { spec };
			let kind = match (node.kind(), spec.child_by_field_name("type").map(|t| t.kind())) {
				("const_declaration", _) => SymbolKind::Const,
				("var_declaration", _) => SymbolKind::Variable,
				(_, Some("struct_type")) => SymbolKind::Struct,
				(_, Some("interface_type")) => SymbolKind::Interface,
				_ => SymbolKind::TypeAlias,
			};
			let mut cursor = spec.walk();
			for name in spec.children_by_field_name("name", &mut cursor) {
				if name.is_named() {
					self.declare(Decl::new(kind, self.text(name), outer));
				}
			}
		}
	}

	/// Receiver base type of a Go method, peeling pointers and generics.
	fn go_receiver(&self, node: Node<'a>) -> Option<&'a str> {
		let params = node.child_by_field_name("receiver")?;
		let mut cursor = params.walk();
		let param = params
			.named_children(&mut cursor)
			.find(|child| child.kind() == "parameter_declaration")?;
		let mut ty = param.child_by_field_name("type")?;
		loop {
			ty = match ty.kind() {
				"type_identifier" => return Some(self.text(ty)),
				"pointer_type" | "parenthesized_type" => ty.named_child(0)?,
				"generic_type" => ty.child_by_field_name("type")?,
				_ => return None,
			};
		}
	}

	fn visit_java(&mut self, node: Node<'a>) {
		let kind = match node.kind() {
			"enum_body_declarations" => return self.visit_children(node),
			"class_declaration" | "record_declaration" => SymbolKind::Class,
			"interface_declaration" | "annotation_type_declaration" => SymbolKind::Interface,
			"enum_declaration" => SymbolKind::Enum,
			"method_declaration" | "constructor_declaration" => SymbolKind::Method,
			_ => return,
		};
		let Some(name) = self.field_text(node, "name") else {
			return;
		};
		let mut decl = Decl::new(kind, name, node);
		if matches!(kind, SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum)
			&& node.kind() != "annotation_type_declaration"
		{
			decl.nested = node
				.child_by_field_name("body")
				.map(|body| (body, ScopeKind::Type));
		}
		self.declare(decl);
	}
}

fn is_script_function(kind: &str) -> bool {
	matches!(kind, "arrow_function" | "function_expression" | "function" | "generator_function")
}

/// Does `chain` end with `query`, segment for segment?
fn ends_with_segments<'a>(chain: impl DoubleEndedIterator<Item = &'a str>, query: &[&str]) -> bool {
	let mut chain = chain.rev();
	query
		.iter()
		.rev()
		.all(|segment| chain.next() == Some(*segment))
}

#[cfg(test)]
mod tests {
	use super::*;

	type Row = (&'static str, SymbolKind, u32, u32);

	fn find(language: SupportLang, source: &str, selector: &str) -> SmallVec<SymbolMatch, 4> {
		let query = SymbolQuery::parse(selector).expect("valid selector");
		find_symbols(source, language, &query).expect("supported language")
	}

	/// `(path, kind, start_line, end_line)` per match, in result order.
	fn rows(found: &[SymbolMatch]) -> Vec<(&str, SymbolKind, u32, u32)> {
		found
			.iter()
			.map(|m| (m.path.as_str(), m.kind, m.start_line, m.end_line))
			.collect()
	}

	fn check(language: SupportLang, source: &str, cases: &[(&str, &[Row])]) {
		for (selector, expected) in cases {
			let found = find(language, source, selector);
			assert_eq!(rows(&found), *expected, "selector `{selector}`");
		}
	}

	use SymbolKind::{
		Class, Const, Enum, Function, Impl, Interface, Method, Module, Static, Struct, Trait,
		TypeAlias, Variable,
	};

	const RUST: &str = "\
//! crate docs
use std::fmt;

/// A point.
#[derive(Debug, Clone)]
pub struct Point {
	x: i32,
}

/// Kind of shape.
pub enum Shape {
	Dot,
}

pub trait Draw {
	/// Draw it.
	fn draw(&self);
	fn name(&self) -> &str {
		\"draw\"
	}
}

impl Point {
	pub const ORIGIN: i32 = 0;

	/// Make one.
	#[inline]
	pub fn new(x: i32) -> Self {
		Self { x }
	}
}

impl Draw for Point {
	fn draw(&self) {}
}

impl fmt::Display for Point {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		Ok(())
	}
}

impl fmt::Debug for Point {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		Ok(())
	}
}

pub mod shapes {
	pub struct Circle;

	impl Circle {
		pub fn area(&self) -> f32 {
			fn helper() {}
			let _c = || 1;
			0.0
		}
	}

	pub fn new() {}
}

// plain comment

static COUNT: u32 = 0;
type Alias = Vec<u8>;

/// Free fn.
pub fn new(a: i32) {}
";

	#[test]
	fn rust_items_include_attributes_and_docs() {
		check(SupportLang::Rust, RUST, &[
			("Shape", &[("Shape", Enum, 10, 13)]),
			("Draw", &[("Draw", Trait, 15, 21)]),
			("shapes", &[("shapes", Module, 49, 61)]),
			("COUNT", &[("COUNT", Static, 65, 65)]),
			("Alias", &[("Alias", TypeAlias, 66, 66)]),
			("ORIGIN", &[("Point.ORIGIN", Const, 24, 24)]),
			("Point.ORIGIN", &[("Point.ORIGIN", Const, 24, 24)]),
		]);
	}

	#[test]
	fn rust_bare_name_returns_every_declaration_sorted() {
		check(SupportLang::Rust, RUST, &[
			("Point", &[
				("Point", Struct, 4, 8),
				("Point", Impl, 23, 31),
				("Point", Impl, 33, 35),
				("Point", Impl, 37, 41),
				("Point", Impl, 43, 47),
			]),
			("new", &[
				("Point.new", Method, 26, 30),
				("new", Function, 60, 60),
				("new", Function, 68, 69),
			]),
			("Circle", &[("Circle", Struct, 50, 50), ("Circle", Impl, 52, 58)]),
		]);
	}

	#[test]
	fn rust_qualified_queries_are_suffixes_of_the_chain() {
		check(SupportLang::Rust, RUST, &[
			("Point.new", &[("Point.new", Method, 26, 30)]),
			("Circle.area", &[("Circle.area", Method, 53, 57)]),
			// A query may start at an enclosing module even though the path omits it.
			("shapes.Circle.area", &[("Circle.area", Method, 53, 57)]),
			("shapes.new", &[("new", Function, 60, 60)]),
			// Suffixes are contiguous: skipping the container does not match.
			("shapes.area", &[]),
			("Other.new", &[]),
			("Draw.draw", &[("Draw.draw", Method, 16, 17)]),
			("Draw.name", &[("Draw.name", Method, 18, 20)]),
		]);
		let nested = find(SupportLang::Rust, RUST, "Circle.area");
		assert_eq!(nested[0].module, "shapes");
		assert_eq!(nested[0].container(), "Circle");
		assert_eq!(nested[0].name(), "area");
	}

	#[test]
	fn rust_trait_impls_are_candidates_distinguished_by_via_trait() {
		let found = find(SupportLang::Rust, RUST, "Point.fmt");
		assert_eq!(rows(&found), [("Point.fmt", Method, 38, 40), ("Point.fmt", Method, 44, 46),]);
		assert_eq!(found[0].via_trait.as_deref(), Some("Display"));
		assert_eq!(found[1].via_trait.as_deref(), Some("Debug"));

		let draw = find(SupportLang::Rust, RUST, "draw");
		assert_eq!(rows(&draw), [("Draw.draw", Method, 16, 17), ("Point.draw", Method, 34, 34)]);
		assert_eq!(draw[0].via_trait, None);
		assert_eq!(draw[1].via_trait.as_deref(), Some("Draw"));

		let impls = find(SupportLang::Rust, RUST, "Point");
		let traits: Vec<_> = impls.iter().map(|m| m.via_trait.as_deref()).collect();
		assert_eq!(traits, [None, None, Some("Draw"), Some("Display"), Some("Debug")]);
	}

	#[test]
	fn rust_nested_functions_and_closures_are_not_addressable() {
		check(SupportLang::Rust, RUST, &[("helper", &[]), ("_c", &[])]);
	}

	#[test]
	fn rust_missing_name_is_empty_not_an_error() {
		assert!(find(SupportLang::Rust, RUST, "does_not_exist").is_empty());
		assert!(find(SupportLang::Rust, "", "x").is_empty());
	}

	#[test]
	fn rust_ranges_and_bytes_slice_the_attached_text() {
		let found = find(SupportLang::Rust, RUST, "Point.new");
		let text = &RUST[found[0].byte_range()];
		assert!(text.starts_with("/// Make one.\n\t#[inline]\n\tpub fn new"), "{text:?}");
		assert!(text.ends_with("\t}"), "{text:?}");
		let strukt = &RUST[find(SupportLang::Rust, RUST, "Shape")[0].byte_range()];
		assert!(strukt.starts_with("/// Kind of shape.\npub enum Shape"), "{strukt:?}");
	}

	#[test]
	fn rust_blank_line_or_plain_comment_detaches() {
		let source = "// plain\nfn a() {}\n\n/// doc\n\nfn b() {}\n\n#[x]\n// plain\nfn c() {}\n";
		check(SupportLang::Rust, source, &[
			("a", &[("a", Function, 2, 2)]),
			("b", &[("b", Function, 6, 6)]),
			("c", &[("c", Function, 10, 10)]),
		]);
	}

	#[test]
	fn rust_trailing_comment_of_previous_item_is_not_attached() {
		let source = "fn a() {} /// trailing\nfn b() {}\n";
		check(SupportLang::Rust, source, &[("b", &[("b", Function, 2, 2)])]);
	}

	#[test]
	fn rust_impl_self_type_forms() {
		let source = "\
impl<T> Wrapper<T> {
	fn a(&self) {}
}
impl Trait for &Other {
	fn b(&self) {}
}
impl<T> fmt::Display for crate::path::Deep<T> {
	fn c(&self) {}
}
";
		check(SupportLang::Rust, source, &[
			("Wrapper.a", &[("Wrapper.a", Method, 2, 2)]),
			("Other.b", &[("Other.b", Method, 5, 5)]),
			("Deep.c", &[("Deep.c", Method, 8, 8)]),
		]);
	}

	#[test]
	fn rust_impl_in_module_and_declared_mod_without_body() {
		let source = "mod a;\nmod m {\n\timpl S {\n\t\t#[napi]\n\t\tfn f(&self) {}\n\t}\n}\n";
		check(SupportLang::Rust, source, &[
			("a", &[("a", Module, 1, 1)]),
			("S.f", &[("S.f", Method, 4, 5)]),
			("m.S.f", &[("S.f", Method, 4, 5)]),
		]);
	}

	#[test]
	fn rust_associated_types_and_trait_consts() {
		let source = "trait T {\n\ttype Item;\n\tconst N: usize;\n}\nimpl T for S {\n\ttype Item = \
		              u8;\n\tconst N: usize = 1;\n}\n";
		check(SupportLang::Rust, source, &[
			("Item", &[("T.Item", TypeAlias, 2, 2), ("S.Item", TypeAlias, 6, 6)]),
			("N", &[("T.N", Const, 3, 3), ("S.N", Const, 7, 7)]),
		]);
	}

	const TYPESCRIPT: &str = "\
/** Adds. */
export function add(a: number, b: number): number {
	return a + b;
}

@sealed
export class Greeter {
	/** Say hi. */
	greet(): string {
		return \"hi\";
	}

	static make = () => new Greeter();
}

export const double = (n: number) => {
	return n * 2;
};

interface Shape {
	area(): number;
}

type Id = string;
enum Color { Red }
namespace NS {
	export function inner() {}
}
const LIMIT = 10;
let counter = 0;
";

	#[test]
	fn typescript_declarations() {
		check(SupportLang::TypeScript, TYPESCRIPT, &[
			("add", &[("add", Function, 1, 4)]),
			("Greeter", &[("Greeter", Class, 6, 14)]),
			("Greeter.greet", &[("Greeter.greet", Method, 8, 11)]),
			("greet", &[("Greeter.greet", Method, 8, 11)]),
			("Greeter.make", &[("Greeter.make", Method, 13, 13)]),
			("double", &[("double", Function, 16, 18)]),
			("Shape", &[("Shape", Interface, 20, 22)]),
			("Shape.area", &[("Shape.area", Method, 21, 21)]),
			("Id", &[("Id", TypeAlias, 24, 24)]),
			("Color", &[("Color", Enum, 25, 25)]),
			("NS", &[("NS", Module, 26, 28)]),
			("NS.inner", &[("inner", Function, 27, 27)]),
			("inner", &[("inner", Function, 27, 27)]),
			("LIMIT", &[("LIMIT", Const, 29, 29)]),
			("counter", &[("counter", Variable, 30, 30)]),
			("missing", &[]),
		]);
		let inner = find(SupportLang::TypeScript, TYPESCRIPT, "inner");
		assert_eq!(inner[0].module, "NS");
	}

	#[test]
	fn tsx_and_javascript_share_the_script_table() {
		let tsx = "function App() {\n\treturn <div/>;\n}\nclass A {\n\tm() {}\n}\n";
		check(SupportLang::Tsx, tsx, &[
			("App", &[("App", Function, 1, 3)]),
			("A.m", &[("A.m", Method, 5, 5)]),
		]);
		let js =
			"/**\n * Doc.\n */\nfunction f() {}\nconst g = function () {};\nclass B {\n\t#p() {}\n}\n";
		check(SupportLang::JavaScript, js, &[
			("f", &[("f", Function, 1, 4)]),
			("g", &[("g", Function, 5, 5)]),
			("B.#p", &[("B.#p", Method, 7, 7)]),
		]);
	}

	const PYTHON: &str = "\
import os


@decorator
@other(1)
def top(a):
	def nested():
		pass
	return a


class Box:
	\"\"\"doc\"\"\"

	@property
	def size(self):
		return 1

	async def load(self):
		pass


if True:
	def cond():
		pass
";

	#[test]
	fn python_decorators_are_part_of_the_range() {
		check(SupportLang::Python, PYTHON, &[
			("top", &[("top", Function, 4, 9)]),
			("nested", &[]),
			("Box", &[("Box", Class, 12, 20)]),
			("Box.size", &[("Box.size", Method, 15, 17)]),
			("size", &[("Box.size", Method, 15, 17)]),
			("Box.load", &[("Box.load", Method, 19, 20)]),
			("cond", &[("cond", Function, 24, 25)]),
		]);
	}

	const GO: &str = "\
package main

// Point is a point.
type Point struct {
	X int
}

// Area returns area.
func (p *Point) Area() int {
	return p.X
}

func (p Point) String() string { return \"\" }

func New() *Point {
	return &Point{}
}

type Shape interface {
	Area() int
}

const (
	A = 1
	B = 2
)

var Count int
";

	#[test]
	fn go_receivers_become_containers() {
		check(SupportLang::Go, GO, &[
			("Point", &[("Point", Struct, 3, 6)]),
			("Point.Area", &[("Point.Area", Method, 8, 11)]),
			("Area", &[("Point.Area", Method, 8, 11)]),
			("Point.String", &[("Point.String", Method, 13, 13)]),
			("New", &[("New", Function, 15, 17)]),
			("Shape", &[("Shape", Interface, 19, 21)]),
			("A", &[("A", Const, 24, 24)]),
			("B", &[("B", Const, 25, 25)]),
			("Count", &[("Count", Variable, 28, 28)]),
			("Shape.Area", &[]),
		]);
	}

	const JAVA: &str = "\
package p;

/** A box. */
@Deprecated
public class Box {
	private int n;

	/** Make. */
	public Box(int n) {
		this.n = n;
	}

	@Override
	public String toString() {
		return \"\";
	}

	static class Inner {
		void run() {}
	}
}

interface Shape {
	double area();
}
";

	#[test]
	fn java_annotations_and_javadoc_are_part_of_the_range() {
		check(SupportLang::Java, JAVA, &[
			("Box", &[("Box", Class, 3, 21), ("Box.Box", Method, 8, 11)]),
			("Box.Box", &[("Box.Box", Method, 8, 11)]),
			("toString", &[("Box.toString", Method, 13, 16)]),
			("Box.Inner", &[("Box.Inner", Class, 18, 20)]),
			("Box.Inner.run", &[("Box.Inner.run", Method, 19, 19)]),
			("Shape.area", &[("Shape.area", Method, 24, 24)]),
		]);
	}

	#[test]
	fn query_shape_is_validated() {
		let cases = [
			("", SymbolQueryError::Empty),
			(".", SymbolQueryError::EmptySegment { index: 0 }),
			("a.", SymbolQueryError::EmptySegment { index: 1 }),
			(".a", SymbolQueryError::EmptySegment { index: 0 }),
			("Point..new", SymbolQueryError::EmptySegment { index: 1 }),
			("a b", SymbolQueryError::WhitespaceInSegment { index: 0 }),
			("a.b c", SymbolQueryError::WhitespaceInSegment { index: 1 }),
		];
		for (selector, expected) in cases {
			assert_eq!(SymbolQuery::parse(selector), Err(expected), "`{selector}`");
		}
		let query = SymbolQuery::parse("mod.Type.method").expect("valid");
		assert_eq!(query.segments(), ["mod", "Type", "method"]);
		assert_eq!(query.name(), "method");
	}

	#[test]
	fn matching_is_case_sensitive() {
		check(SupportLang::Rust, RUST, &[("point", &[]), ("Point.New", &[])]);
	}

	#[test]
	fn unsupported_language_is_a_typed_error() {
		let query = SymbolQuery::parse("x").expect("valid");
		for language in [SupportLang::Json, SupportLang::Toml, SupportLang::Bash] {
			let error = find_symbols("x", language, &query).expect_err("no declaration table");
			assert!(
				matches!(error, SymbolError::UnsupportedLanguage { language: l } if l == language),
				"{error:?}"
			);
		}
	}

	#[test]
	fn kind_strings_are_kebab_case() {
		assert_eq!(SymbolKind::TypeAlias.to_string(), "type-alias");
		assert_eq!(<&str>::from(SymbolKind::Method), "method");
		assert_eq!("type-alias".parse(), Ok(SymbolKind::TypeAlias));
	}
	/// Every match's name offset must land on its own identifier, and the
	/// derived point must index the same bytes through line and column.
	fn assert_names_located(language: SupportLang, source: &str, selectors: &[&str]) {
		for selector in selectors {
			let found = find(language, source, selector);
			assert!(!found.is_empty(), "`{selector}` must match");
			for matched in &found {
				let name = matched.name();
				assert_eq!(
					source.get(matched.name_byte..matched.name_byte + name.len()),
					Some(name),
					"`{selector}` -> {}",
					matched.path
				);
				let point = matched.name_point(source).expect("same source");
				let line = source
					.split('\n')
					.nth(point.line as usize - 1)
					.expect("line exists");
				assert!(
					line
						.get(point.column as usize..)
						.is_some_and(|rest| rest.starts_with(name)),
					"`{selector}` -> {} at {point:?} in {line:?}",
					matched.path
				);
				assert!(
					(matched.start_line..=matched.end_line).contains(&point.line),
					"the name lies inside the declaration's range"
				);
			}
		}
	}

	#[test]
	fn name_point_marks_the_identifier_not_the_attached_attributes() {
		let found = find(SupportLang::Rust, RUST, "Point.new");
		let [matched] = found.as_slice() else {
			panic!("one match: {found:?}");
		};
		// Doc comment and `#[inline]` start the range two lines above the name.
		assert_eq!((matched.start_line, matched.end_line), (26, 30));
		assert_eq!(matched.name_point(RUST), Some(SourcePoint { line: 28, column: 8 }));
		assert_eq!(&RUST[matched.name_byte..][..3], "new");
	}

	#[test]
	fn name_points_are_exact_in_every_supported_language() {
		assert_names_located(SupportLang::Rust, RUST, &[
			"Point",
			"Point.new",
			"ORIGIN",
			"Draw",
			"draw",
			"fmt",
			"Shape",
		]);
		assert_names_located(SupportLang::TypeScript, TYPESCRIPT, &[
			"add",
			"Greeter",
			"Shape",
			"Shape.area",
			"Id",
			"Color",
			"NS",
			"inner",
			"LIMIT",
			"counter",
		]);
		assert_names_located(SupportLang::Python, PYTHON, &["load", "size", "cond"]);
		assert_names_located(SupportLang::Go, GO, &["Point", "Point.Area", "New", "Count", "A"]);
		assert_names_located(SupportLang::Java, JAVA, &[
			"Box",
			"Box.Box",
			"toString",
			"Box.Inner",
			"Box.Inner.run",
			"Shape.area",
		]);
		assert_names_located(
			SupportLang::Tsx,
			"function App() {\n\treturn <div/>;\n}\nclass A {\n\tm() {}\n}\n",
			&["App", "A.m"],
		);
	}

	#[test]
	fn name_points_count_bytes_after_wide_characters_and_survive_crlf() {
		let source =
			"// h\u{e9}llo \u{1F600}\r\nfn caf\u{e9}() {}\r\nconst \u{c9}: u8 = 1; fn after() {}\r\n";
		let found = find(SupportLang::Rust, source, "after");
		let [matched] = found.as_slice() else {
			panic!("one match: {found:?}");
		};
		assert_eq!(
			matched.name_point(source),
			Some(SourcePoint { line: 3, column: 21 }),
			"the column counts bytes, so the two-byte capital E acute widens it past 20 characters"
		);
		let accent = find(SupportLang::Rust, source, "caf\u{e9}");
		assert_eq!(accent[0].name_point(source), Some(SourcePoint { line: 2, column: 3 }));
		assert_names_located(SupportLang::Rust, source, &["after", "caf\u{e9}", "\u{c9}"]);
	}

	#[test]
	fn name_point_rejects_text_that_is_not_the_searched_source() {
		let found = find(SupportLang::Rust, RUST, "Point.new");
		assert_eq!(found[0].name_point("fn x() {}"), None, "shorter than the name offset");
		assert_eq!(found[0].name_point(""), None);
	}
}
