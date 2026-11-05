//! Tree-sitter parser functions for all supported languages.
//!
//! Research harness only (never merged): each grammar sits behind a Cargo
//! feature.

use ast_grep_core::tree_sitter::TSLanguage;

macro_rules! parsers {
	($($feature:tt => $name:ident = $value:expr;)*) => {$(
		/// Returns the tree-sitter language for this parser.
		#[cfg(feature = $feature)]
		pub fn $name() -> TSLanguage {
			$value
		}

		/// Panics: this build does not compile the grammar in.
		#[cfg(not(feature = $feature))]
		pub fn $name() -> TSLanguage {
			panic!(concat!(stringify!($name), ": grammar not compiled into this build"))
		}
	)*};
}

parsers! {
	"grammars-all" => language_astro = tree_sitter_astro::LANGUAGE.into();
	"grammars-hot" => language_bash = tree_sitter_bash::LANGUAGE.into();
	"grammars-hot" => language_c = tree_sitter_c::LANGUAGE.into();
	"grammars-all" => language_clojure = tree_sitter_clojure::LANGUAGE.into();
	"grammars-all" => language_cmake = tree_sitter_cmake::LANGUAGE.into();
	"grammars-hot" => language_cpp = tree_sitter_cpp::LANGUAGE.into();
	"grammars-all" => language_c_sharp = tree_sitter_c_sharp::LANGUAGE.into();
	"grammars-all" => language_dart = tree_sitter_dart::LANGUAGE.into();
	"grammars-hot" => language_css = tree_sitter_css::LANGUAGE.into();
	"grammars-hot" => language_diff = tree_sitter_diff::LANGUAGE.into();
	"grammars-all" => language_dockerfile = tree_sitter_dockerfile::language();
	"grammars-all" => language_elixir = tree_sitter_elixir::LANGUAGE.into();
	"grammars-all" => language_elisp = tree_sitter_elisp::LANGUAGE.into();
	"grammars-all" => language_erlang = tree_sitter_erlang::LANGUAGE.into();
	"grammars-all" => language_fortran = tree_sitter_fortran::LANGUAGE.into();
	"grammars-hot" => language_go = tree_sitter_go::LANGUAGE.into();
	"grammars-all" => language_graphql = tree_sitter_graphql::LANGUAGE.into();
	"grammars-all" => language_haskell = tree_sitter_haskell::LANGUAGE.into();
	"grammars-all" => language_hcl = tree_sitter_hcl::LANGUAGE.into();
	"grammars-hot" => language_html = tree_sitter_html::LANGUAGE.into();
	"grammars-all" => language_ini = tree_sitter_ini::LANGUAGE.into();
	"grammars-hot" => language_java = tree_sitter_java::LANGUAGE.into();
	"grammars-hot" => language_javascript = tree_sitter_javascript::LANGUAGE.into();
	"grammars-hot" => language_json = tree_sitter_json::LANGUAGE.into();
	"grammars-all" => language_just = tree_sitter_just::LANGUAGE.into();
	"grammars-all" => language_julia = tree_sitter_julia::LANGUAGE.into();
	"grammars-all" => language_kotlin = tree_sitter_kotlin::LANGUAGE.into();
	"grammars-all" => language_lua = tree_sitter_lua::LANGUAGE.into();
	"grammars-all" => language_make = tree_sitter_make::LANGUAGE.into();
	"grammars-hot" => language_markdown = tree_sitter_md::LANGUAGE.into();
	"grammars-all" => language_nix = tree_sitter_nix::LANGUAGE.into();
	"grammars-all" => language_objc = tree_sitter_objc::LANGUAGE.into();
	"grammars-all" => language_ocaml = tree_sitter_ocaml::LANGUAGE_OCAML.into();
	"grammars-all" => language_odin = tree_sitter_odin::LANGUAGE.into();
	"grammars-all" => language_php = tree_sitter_php::LANGUAGE_PHP_ONLY.into();
	"grammars-all" => language_powershell = tree_sitter_powershell::LANGUAGE.into();
	"grammars-all" => language_proto = tree_sitter_proto::LANGUAGE.into();
	"grammars-hot" => language_python = tree_sitter_python::LANGUAGE.into();
	"grammars-all" => language_r = tree_sitter_r::LANGUAGE.into();
	"grammars-hot" => language_regex = tree_sitter_regex::LANGUAGE.into();
	"grammars-all" => language_ruby = tree_sitter_ruby::LANGUAGE.into();
	"grammars-hot" => language_rust = tree_sitter_rust::LANGUAGE.into();
	"grammars-all" => language_scala = tree_sitter_scala::LANGUAGE.into();
	"grammars-all" => language_solidity = tree_sitter_solidity::LANGUAGE.into();
	"grammars-all" => language_sql = tree_sitter_sql::LANGUAGE.into();
	"grammars-all" => language_starlark = tree_sitter_starlark::LANGUAGE.into();
	"grammars-all" => language_svelte = tree_sitter_svelte::LANGUAGE.into();
	"grammars-all" => language_swift = tree_sitter_swift::LANGUAGE.into();
	"grammars-hot" => language_toml = tree_sitter_toml_ng::LANGUAGE.into();
	"grammars-hot" => language_tsx = tree_sitter_typescript::LANGUAGE_TSX.into();
	"grammars-hot" => language_typescript = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
	"grammars-all" => language_tlaplus = tree_sitter_tlaplus::LANGUAGE.into();
	"grammars-all" => language_verilog = tree_sitter_verilog::LANGUAGE.into();
	"grammars-all" => language_vue = tree_sitter_vue::LANGUAGE.into();
	"grammars-all" => language_xml = tree_sitter_xml::LANGUAGE_XML.into();
	"grammars-hot" => language_yaml = tree_sitter_yaml::LANGUAGE.into();
	"grammars-all" => language_zig = tree_sitter_zig::LANGUAGE.into();
}
