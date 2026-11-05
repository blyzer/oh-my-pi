//! Research probe (never merged): links the wasmtime runtime the way a WASM
//! grammar loader uses it.

use tree_sitter::{Parser, WasmStore, wasmtime::Engine};

fn main() {
	let mut args = std::env::args().skip(1);
	let path = args
		.next()
		.expect("usage: wasm_runtime_size MODULE.wasm SYMBOL");
	let symbol = args
		.next()
		.expect("usage: wasm_runtime_size MODULE.wasm SYMBOL");
	let bytes = std::fs::read(path).expect("wasm module");
	let engine = Engine::default();
	let language = WasmStore::new(&engine)
		.expect("wasm store")
		.load_language(&symbol, &bytes)
		.expect("load_language");
	let mut parser = Parser::new();
	parser
		.set_wasm_store(WasmStore::new(&engine).expect("wasm store"))
		.expect("set_wasm_store");
	parser.set_language(&language).expect("set_language");
	let tree = parser.parse("key = value\n", None).expect("parse");
	println!("{}", tree.root_node().to_sexp());
}
