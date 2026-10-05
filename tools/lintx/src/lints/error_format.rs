//! `error-str-payload` / `error-format`: the two error-handling patterns that
//! `AGENTS.md` ("Composition/errors/state") rejects and that ADR 0035 tracks
//! with a per-crate ratchet (see [`crate::ratchet`]). Neither rule runs in the
//! default `lintx` pass: there are hundreds of existing sites, so they are
//! counted against a committed baseline instead of reported one by one.
//!
//! # `error-str-payload`
//!
//! A `thiserror`-style error type that carries a message string instead of a
//! typed cause. Counted, one finding per type or variant:
//!
//! - an enum variant, or a struct, annotated `#[error("…")]` (not
//!   `#[error(transparent)]`) whose **only field is a string**: a tuple field
//!   typed `Str`, `String`, `&str` or `&'static str` (`Foo(Str)`), or a record
//!   field of such a type whose name is a catch-all (`reason`, `message`,
//!   `msg`, `detail`, `details`, `text`, `error`, `cause`, `description`,
//!   `context`, `why`, `info`).
//! - A field marked `#[from]` or `#[source]` is a typed inner error and never
//!   counts. A record variant whose single string field has an identifying name
//!   (`UnknownModel { id: Str }`), or a variant with more than one field, never
//!   counts: those carry named facts.
//!
//! Some tuple payloads are identifiers rather than stringified errors, so the
//! count is an upper bound; the baseline only needs it to be stable.
//!
//! # `error-format`
//!
//! An error value passed through a formatter. Counted, one finding per site.
//! An *error binding* is the first parameter of a closure passed to
//! `map_err`, `or_else`, `unwrap_or_else`, `inspect_err` or (first closure
//! only) `map_or_else`, or the identifier inside an `Err(ident)` pattern of a
//! `match` arm or an `if let`. Inside the scope of that binding (closure body,
//! arm expression, `then` block), a finding is:
//!
//! - `<binding>.to_string()`;
//! - a `format!`, `sf!` or `fmts!` call whose arguments name the binding
//!   (`format!("x: {}", e)`) or inline-capture it (`sf!("x: {e}")`,
//!   `{error:?}`).
//!
//! Logging macros (`tracing::warn!(%error, …)`), `write!` into a `Display`
//! impl, and formatting of values that were not bound as an error never count.
//!
//! # Scope and escapes
//!
//! Test code never counts: `#[test]` functions, any item gated by
//! `#[cfg(test)]`, and (in [`crate::ratchet`]) `tests/`, `benches/` and
//! `examples/` trees, `build.rs` and `tests.rs`/`*_tests.rs` files. A site that
//! is genuinely the render-once boundary (the app crate printing an error)
//! may carry `// lintx-allow: error-format <reason>` on its line or the line
//! above, and likewise `// lintx-allow: error-str-payload <reason>` on the
//! line of (or above) the `#[error(…)]` attribute.

use std::{collections::HashSet, ops::Range};

use ra_ap_syntax::{
	AstNode, NodeOrToken, SyntaxNode,
	ast::{self, HasAttrs, HasName},
};

use crate::{
	fix::PathFix,
	lint::{Diagnosis, FileContext, Lint, RealtimeSink},
};

/// The `error-str-payload` lint; no configuration.
pub struct ErrorStrPayload;
/// The `error-format` lint; no configuration.
pub struct ErrorFormat;

/// One counted site of either rule.
pub struct Finding {
	span:    Range<usize>,
	message: String,
}

impl Diagnosis for Finding {
	fn span(&self) -> Range<usize> {
		self.span.clone()
	}

	fn message(&self) -> String {
		self.message.clone()
	}

	fn autofixable(&self) -> bool {
		false
	}

	fn fix(self) -> Option<PathFix> {
		None
	}
}

/// Record-field names that make a lone string field a catch-all payload.
const CATCH_ALL_FIELDS: &[&str] = &[
	"reason",
	"message",
	"msg",
	"detail",
	"details",
	"text",
	"error",
	"cause",
	"description",
	"context",
	"why",
	"info",
];

/// Method names whose closure argument receives the error value.
const ERROR_CLOSURE_METHODS: &[&str] =
	&["map_err", "or_else", "unwrap_or_else", "inspect_err", "map_or_else"];

/// Macros that render their arguments into a fresh string.
const FORMAT_MACROS: &[&str] = &["format", "sf", "fmts"];

impl Lint for ErrorStrPayload {
	type Instance = Finding;

	const NAME: &'static str = "error-str-payload";

	fn detect(&self, ctx: &FileContext<'_>, sink: &mut RealtimeSink<'_, Finding>) {
		for node in ctx.tree.syntax().descendants() {
			let (attrs, fields, label) = if let Some(variant) = ast::Variant::cast(node.clone()) {
				(variant.attrs().collect::<Vec<_>>(), variant.field_list(), "variant")
			} else if let Some(item) = ast::Struct::cast(node.clone()) {
				(item.attrs().collect::<Vec<_>>(), item.field_list(), "struct")
			} else {
				continue;
			};
			let Some(error_attr) = message_attr(&attrs) else {
				continue;
			};
			let Some(fields) = fields else { continue };
			if !lone_string_payload(&fields) {
				continue;
			}
			// Anchored on `#[error(…)]`, not the item (its doc comments come first).
			let span = range(error_attr.syntax());
			if in_test_code(&node) || allowed(ctx, &span, Self::NAME) {
				continue;
			}
			sink.push(Finding {
				span,
				message: format!(
					"{label} carries a bare string payload; carry the typed inner error \
					 (`#[source]`/`#[from]`) or named identifying facts"
				),
			});
		}
	}
}

impl Lint for ErrorFormat {
	type Instance = Finding;

	const NAME: &'static str = "error-format";

	fn detect(&self, ctx: &FileContext<'_>, sink: &mut RealtimeSink<'_, Finding>) {
		let mut seen: HashSet<usize> = HashSet::new();
		for node in ctx.tree.syntax().descendants() {
			let Some((name, scope)) = error_binding(&node) else {
				continue;
			};
			for site in formatted_sites(&scope, &name) {
				let span = range(&site);
				if !seen.insert(span.start) || in_test_code(&site) || allowed(ctx, &span, Self::NAME) {
					continue;
				}
				sink.push(Finding {
					span,
					message: format!(
						"error `{name}` passes through a formatter; keep it typed and render once at \
						 the boundary"
					),
				});
			}
		}
	}
}

fn range(node: &SyntaxNode) -> Range<usize> {
	let range = node.text_range();
	range.start().into()..range.end().into()
}

/// `#[error("…")]`, excluding `#[error(transparent)]`.
fn message_attr(attrs: &[ast::Attr]) -> Option<&ast::Attr> {
	attrs.iter().find(|attr| {
		attr
			.as_simple_call()
			.is_some_and(|(name, tree)| name == "error" && !is_transparent(&tree))
	})
}

fn is_transparent(tree: &ast::TokenTree) -> bool {
	let text = tree.syntax().text().to_string();
	text
		.trim_start_matches('(')
		.trim_start()
		.starts_with("transparent")
}

/// Whether a field list is exactly one unmarked string field (tuple form) or
/// one catch-all-named string field (record form).
fn lone_string_payload(fields: &ast::FieldList) -> bool {
	match fields {
		ast::FieldList::TupleFieldList(list) => {
			let mut it = list.fields();
			let (Some(field), None) = (it.next(), it.next()) else {
				return false;
			};
			!has_source_attr(field.attrs()) && field.ty().is_some_and(|ty| is_string_type(&ty))
		},
		ast::FieldList::RecordFieldList(list) => {
			let mut it = list.fields();
			let (Some(field), None) = (it.next(), it.next()) else {
				return false;
			};
			let named_catch_all = field
				.name()
				.is_some_and(|name| CATCH_ALL_FIELDS.contains(&name.text().to_string().as_str()));
			named_catch_all
				&& !has_source_attr(field.attrs())
				&& field.ty().is_some_and(|ty| is_string_type(&ty))
		},
	}
}

fn has_source_attr(mut attrs: impl Iterator<Item = ast::Attr>) -> bool {
	attrs.any(|attr| matches!(attr.simple_name().as_deref(), Some("from" | "source")))
}

/// `Str`, `String`, `&str`, `&'static str`.
fn is_string_type(ty: &ast::Type) -> bool {
	match ty {
		ast::Type::PathType(path) => path
			.path()
			.and_then(|path| path.segment())
			.and_then(|segment| segment.name_ref())
			.is_some_and(|name| matches!(name.text().to_string().as_str(), "Str" | "String")),
		ast::Type::RefType(reference) => reference.ty().is_some_and(|inner| {
			matches!(&inner, ast::Type::PathType(path)
				if path.syntax().text().to_string() == "str")
		}),
		_ => false,
	}
}

/// If `node` introduces an error binding, its name and the syntax it scopes.
fn error_binding(node: &SyntaxNode) -> Option<(String, SyntaxNode)> {
	if let Some(closure) = ast::ClosureExpr::cast(node.clone()) {
		return closure_binding(&closure);
	}
	if let Some(arm) = ast::MatchArm::cast(node.clone()) {
		let name = err_pattern_binding(&arm.pat()?)?;
		return Some((name, arm.expr()?.syntax().clone()));
	}
	if let Some(branch) = ast::IfExpr::cast(node.clone()) {
		let ast::Expr::LetExpr(condition) = branch.condition()? else {
			return None;
		};
		let name = err_pattern_binding(&condition.pat()?)?;
		return Some((name, branch.then_branch()?.syntax().clone()));
	}
	None
}

fn closure_binding(closure: &ast::ClosureExpr) -> Option<(String, SyntaxNode)> {
	let args = closure.syntax().parent().and_then(ast::ArgList::cast)?;
	let call = args.syntax().parent().and_then(ast::MethodCallExpr::cast)?;
	let method = call.name_ref()?;
	if !ERROR_CLOSURE_METHODS.contains(&method.text().to_string().as_str()) {
		return None;
	}
	if method.text().to_string() == "map_or_else" && args.args().next()?.syntax() != closure.syntax()
	{
		return None;
	}
	let param = closure.param_list()?.params().next()?;
	let ast::Pat::IdentPat(ident) = param.pat()? else {
		return None;
	};
	let name = ident.name()?.text().to_string();
	(name != "_").then(|| (name, closure.syntax().clone()))
}

/// `e` in an `Err(e)` pattern.
fn err_pattern_binding(pat: &ast::Pat) -> Option<String> {
	let ast::Pat::TupleStructPat(tuple) = pat else {
		return None;
	};
	let tail = tuple.path()?.segment()?.name_ref()?;
	if tail.text().to_string() != "Err" {
		return None;
	}
	let mut fields = tuple.fields();
	let (Some(ast::Pat::IdentPat(ident)), None) = (fields.next(), fields.next()) else {
		return None;
	};
	let name = ident.name()?.text().to_string();
	(name != "_").then_some(name)
}

/// Syntax nodes inside `scope` that format the binding `name`.
fn formatted_sites(scope: &SyntaxNode, name: &str) -> Vec<SyntaxNode> {
	let mut sites = Vec::new();
	for node in scope.descendants() {
		if let Some(call) = ast::MethodCallExpr::cast(node.clone()) {
			let is_to_string = call
				.name_ref()
				.is_some_and(|method| method.text().to_string() == "to_string");
			let on_binding = call
				.receiver()
				.is_some_and(|receiver| receiver.syntax().text().to_string() == name);
			if is_to_string && on_binding {
				sites.push(node);
			}
		} else if let Some(call) = ast::MacroCall::cast(node.clone())
			&& call
				.path()
				.and_then(|path| path.segment())
				.and_then(|segment| segment.name_ref())
				.is_some_and(|tail| FORMAT_MACROS.contains(&tail.text().to_string().as_str()))
			&& call
				.token_tree()
				.is_some_and(|tree| names_binding(tree.syntax(), name))
		{
			sites.push(node);
		}
	}
	sites
}

/// Whether a macro token tree passes `name` as an argument or captures it
/// inline in a format string.
fn names_binding(tree: &SyntaxNode, name: &str) -> bool {
	tree.descendants_with_tokens().any(|element| {
		let NodeOrToken::Token(token) = element else {
			return false;
		};
		match token.kind() {
			ra_ap_syntax::SyntaxKind::IDENT => token.text() == name,
			ra_ap_syntax::SyntaxKind::STRING => inline_capture(token.text(), name),
			_ => false,
		}
	})
}

/// `{name}` or `{name:…}` inside a string literal (`{{` escapes skipped).
fn inline_capture(literal: &str, name: &str) -> bool {
	let bytes = literal.as_bytes();
	let mut index = 0;
	while index < bytes.len() {
		if bytes[index] == b'{' {
			if bytes.get(index + 1) == Some(&b'{') {
				index += 2;
				continue;
			}
			let rest = &literal[index + 1..];
			if let Some(after) = rest.strip_prefix(name)
				&& after.starts_with(['}', ':'])
			{
				return true;
			}
		}
		index += 1;
	}
	false
}

/// `// lintx-allow: <rule>` on the finding's first line or the line above.
fn allowed(ctx: &FileContext<'_>, span: &Range<usize>, rule: &str) -> bool {
	let (line, _) = ctx.position(span.start);
	let marker = format!("lintx-allow: {rule}");
	ctx.line(line).contains(&marker) || (line > 1 && ctx.line(line - 1).contains(&marker))
}

/// Whether `node` sits in a `#[test]` function or an item gated by `cfg(test)`.
fn in_test_code(node: &SyntaxNode) -> bool {
	node
		.ancestors()
		.filter_map(ast::AnyHasAttrs::cast)
		.any(|item| item.attrs().any(|attr| is_test_attr(&attr)))
}

fn is_test_attr(attr: &ast::Attr) -> bool {
	let is_test_fn = attr
		.path()
		.and_then(|path| path.segment())
		.and_then(|segment| segment.name_ref())
		.is_some_and(|tail| tail.text().to_string() == "test");
	let text = attr.syntax().text().to_string();
	let is_cfg_test =
		text.starts_with("#[cfg(") && text.contains("test") && !text.contains("not(test");
	is_test_fn || is_cfg_test
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;
	use crate::lint::AnyLint;

	fn count(lint: &dyn AnyLint, source: &str) -> usize {
		let ctx = FileContext::new(Path::new("sample.rs"), source);
		let mut found = 0;
		lint.detect_erased(&ctx, &mut |_| found += 1);
		found
	}

	#[test]
	fn tuple_string_payloads_are_counted() {
		let source = r#"
			enum E {
				#[error("a: {0}")] A(Str),
				#[error("b: {0}")]
				B(String),
				#[error("c: {0}")] C(&'static str),
				#[error("d")] D,
			}
		"#;
		assert_eq!(count(&ErrorStrPayload, source), 3);
	}

	#[test]
	fn typed_causes_and_identifying_facts_are_not_counted() {
		let source = r#"
			enum E {
				#[error("io")] Io(#[from] std::io::Error),
				#[error("io")] Wrapped(#[source] String),
				#[error(transparent)] Inner(String),
				#[error("two")] Two(Str, u32),
				#[error("missing {id}")] Missing { id: Str },
				#[error("{path}: {reason}")] Both { path: Str, reason: Str },
				#[error("n")] Number(u32),
				Plain(Str),
			}
		"#;
		assert_eq!(count(&ErrorStrPayload, source), 0);
	}

	#[test]
	fn record_catch_all_names_and_structs_are_counted() {
		let source = r#"
			enum E {
				#[error("bad: {reason}")] Bad { reason: Str },
				#[error("bad: {message}")] Worse { message: String },
			}
			#[error("wrapper: {0}")]
			struct S(String);
			#[error("fine")]
			struct T { id: Str }
		"#;
		assert_eq!(count(&ErrorStrPayload, source), 3);
	}

	#[test]
	fn test_code_and_allow_markers_are_exempt_from_payload_rule() {
		let source = r#"
			#[cfg(test)]
			mod tests {
				enum E { #[error("x")] X(Str) }
			}
			enum F {
				// lintx-allow: error-str-payload message is already redacted
				#[error("y: {0}")] Y(Str),
				#[error("z: {0}")] Z(Str),
			}
		"#;
		assert_eq!(count(&ErrorStrPayload, source), 1);
	}

	#[test]
	fn map_err_closures_that_format_the_error_are_counted() {
		let source = r#"
			fn f() {
				a().map_err(|e| E::A(e.to_string()));
				b().map_err(|error| E::B(format!("b: {error}")));
				c().map_err(|err| E::C(sf!("c: {}", err)));
				d().map_err(|e| Str::new(format!("d: {e:?}")));
				e().unwrap_or_else(|e| e.to_string());
			}
		"#;
		assert_eq!(count(&ErrorFormat, source), 5);
	}

	#[test]
	fn typed_error_handling_is_not_counted() {
		let source = r#"
			fn f() {
				a().map_err(E::A);
				b().map_err(|source| E::B { source });
				c().map_err(|e| { tracing::warn!(%e, "failed"); E::C });
				d().map_err(|_| E::D);
				e().map_err(|e| format!("static text"));
				let name = "x"; let _ = name.to_string();
				g().map_err(|e| E::G(other.to_string()));
				h().map_err(|e| { let _ = write!(out, "{e}"); E::H });
			}
		"#;
		assert_eq!(count(&ErrorFormat, source), 0);
	}

	#[test]
	fn err_arms_and_if_let_bind_errors() {
		let source = r#"
			fn f() {
				match run() {
					Ok(v) => v,
					Err(e) => return Err(E::Run(e.to_string())),
				}
				if let Err(error) = run() {
					log(format!("failed: {}", error));
				}
				match run() {
					Ok(v) => v.to_string(),
					Err(_) => String::new(),
				}
			}
		"#;
		assert_eq!(count(&ErrorFormat, source), 2);
	}

	#[test]
	fn nested_closures_are_counted_once_and_tests_are_exempt() {
		let source = r#"
			fn f() {
				a().map_err(|e| b().map_err(|e| e.to_string()));
			}
			// lintx-allow: error-format boundary render
			fn g() { a().map_err(|e| e.to_string()); }
			fn g2() { a().map_err(|e| e.to_string()); }
			fn h() {
				// lintx-allow: error-format boundary render
				a().map_err(|e| e.to_string());
			}
			#[test]
			fn t() { a().map_err(|e| e.to_string()); }
			#[cfg(test)]
			mod tests { fn u() { a().map_err(|e| e.to_string()); } }
		"#;
		// `f` is one site (the nested closure is not counted twice); `g` and
		// `h` carry a marker on the site's line or the line above; `g2` has
		// none; the test items are exempt.
		assert_eq!(count(&ErrorFormat, source), 2);
	}

	#[test]
	fn inline_capture_ignores_escapes_and_prefixes() {
		assert!(inline_capture("\"x {e}\"", "e"));
		assert!(inline_capture("\"x {e:?}\"", "e"));
		assert!(!inline_capture("\"x {{e}}\"", "e"));
		assert!(!inline_capture("\"x {err}\"", "e"));
	}
}
