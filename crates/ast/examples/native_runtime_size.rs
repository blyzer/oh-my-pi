//! Research probe (never merged): wasm_runtime_size without the wasm calls, as
//! its size baseline.

use tree_sitter::Parser;

fn main() {
	let source = std::env::args()
		.nth(1)
		.unwrap_or_else(|| "{\"key\": \"value\"}".to_owned());
	let mut parser = Parser::new();
	parser
		.set_language(&tree_sitter_json::LANGUAGE.into())
		.expect("set_language");
	let tree = parser.parse(&source, None).expect("parse");
	println!("{}", tree.root_node().to_sexp());
}
