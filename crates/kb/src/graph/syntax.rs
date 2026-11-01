use std::{collections::BTreeMap, path::Path};

use ast_grep_core::tree_sitter::LanguageExt;
use omp_ast::SupportLang;
use tree_sitter::{Language, Node, Parser};

use super::{Call, Implementation, Index, Location, Resolution, token};
use crate::error::{Context, Error, Result};

const FUNCTIONS: [&str; 6] = [
	"function_item",
	"function_definition",
	"function_declaration",
	"method_definition",
	"method_declaration",
	"constructor_declaration",
];

const TYPES: [&str; 8] = [
	"class_definition",
	"class_declaration",
	"struct_item",
	"trait_item",
	"enum_item",
	"enum_declaration",
	"interface_declaration",
	"type_spec",
];

const MAX_SOURCE_BYTES: u64 = 1 << 20;

pub(super) fn is_function(kind: &str) -> bool {
	FUNCTIONS.contains(&kind)
}

fn is_declaration(kind: &str) -> bool {
	is_function(kind) || TYPES.contains(&kind)
}

fn declares(grammar: &Language) -> bool {
	FUNCTIONS
		.iter()
		.chain(&TYPES)
		.any(|kind| grammar.id_for_node_kind(kind, true) != 0)
}

struct File<'a> {
	path:   &'a str,
	source: &'a [u8],
}

struct PendingCall {
	caller:   String,
	callee:   String,
	location: Location,
}

struct PendingImplementation {
	interface: String,
	target:    String,
	location:  Location,
}

#[derive(Default)]
struct Found {
	names:           BTreeMap<String, Vec<String>>,
	calls:           Vec<PendingCall>,
	implementations: Vec<PendingImplementation>,
}

fn declare(index: &mut Index, found: &mut Found, path: &str, name: Node<'_>, text: &str) -> String {
	let point = name.start_position();
	let id = format!("syntactic:{path}:{}:{}:{text}", point.row + 1, point.column + 1);
	let symbol = index.symbols.entry(id.clone()).or_default();
	symbol.resolution = Resolution::Syntactic;
	symbol.name_if_empty(text);
	symbol
		.definitions
		.insert(Location::new(path, point.row, point.column));
	found
		.names
		.entry(text.to_owned())
		.or_default()
		.push(id.clone());
	id
}

fn collect(
	index: &mut Index,
	found: &mut Found,
	file: &File<'_>,
	node: Node<'_>,
	caller: Option<&str>,
) {
	let kind = node.kind();
	let id = is_declaration(kind)
		.then(|| node.child_by_field_name("name"))
		.flatten()
		.and_then(|name| Some((name, name.utf8_text(file.source).ok()?)))
		.map(|(name, text)| declare(index, found, file.path, name, text));
	let scope = match &id {
		Some(id) if is_function(kind) => Some(id.as_str()),
		_ => caller,
	};
	if matches!(kind, "call" | "call_expression")
		&& let (Some(caller), Some(function)) = (scope, node.child_by_field_name("function"))
	{
		let name = token(function);
		if let Ok(callee) = name.utf8_text(file.source) {
			let point = name.start_position();
			found.calls.push(PendingCall {
				caller:   caller.to_owned(),
				callee:   callee.to_owned(),
				location: Location::new(file.path, point.row, point.column),
			});
		}
	}
	if kind == "impl_item"
		&& let (Some(interface), Some(target)) =
			(node.child_by_field_name("trait"), node.child_by_field_name("type"))
		&& let (Ok(interface), Ok(target)) =
			(token(interface).utf8_text(file.source), target.utf8_text(file.source))
	{
		let point = node.start_position();
		found.implementations.push(PendingImplementation {
			interface: interface.to_owned(),
			target:    target.to_owned(),
			location:  Location::new(file.path, point.row, point.column),
		});
	}
	let mut cursor = node.walk();
	for child in node.named_children(&mut cursor) {
		collect(index, found, file, child, scope);
	}
}

fn resolve(index: &mut Index, found: Found) {
	let Found { names, calls, implementations } = found;
	let unique = |name: &str| match names.get(name).map(Vec::as_slice) {
		Some([id]) => Some(id.as_str()),
		_ => None,
	};
	for call in calls {
		let Some(callee) = unique(&call.callee) else {
			continue;
		};
		if let Some(symbol) = index.symbols.get_mut(callee) {
			symbol.references.insert(call.location.clone());
		}
		index.calls.insert(Call {
			caller:   call.caller,
			callee:   callee.to_owned(),
			location: call.location,
		});
	}
	for implementation in implementations {
		let Some(interface) = unique(&implementation.interface) else {
			continue;
		};
		let symbol = unique(&implementation.target).unwrap_or("").to_owned();
		index
			.implementations
			.entry(interface.to_owned())
			.or_default()
			.insert(Implementation {
				symbol,
				name: implementation.target,
				location: implementation.location,
			});
	}
}

impl Index {
	pub(crate) fn add_syntactic(&mut self, root: &Path, paths: &[&str]) -> Result<()> {
		let mut found = Found::default();
		for path in paths {
			let Some(language) = SupportLang::from_path(Path::new(path)) else {
				continue;
			};
			let grammar = language.get_ts_language();
			if !declares(&grammar) {
				continue;
			}
			let full = root.join(path);
			let metadata = std::fs::symlink_metadata(&full).context(format!("reading {path}"))?;
			if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
				continue;
			}
			let source = std::fs::read(&full).context(format!("reading {path}"))?;
			let mut parser = Parser::new();
			parser
				.set_language(&grammar)
				.map_err(|error| Error::foreign("setting parser language", error))?;
			let tree = parser.parse(&source, None).context("cannot parse source")?;
			collect(self, &mut found, &File { path, source: &source }, tree.root_node(), None);
		}
		resolve(self, found);
		Ok(())
	}
}
