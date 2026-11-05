//! Research probe (never merged): first-use latency and parse cost of the
//! stencil-hq/wasm-grammars v1 modules against the natively compiled grammar
//! crates. Prints one tab-separated line per sample.

use std::{
	env,
	fmt::Write as _,
	fs,
	path::Path,
	str::FromStr,
	time::{Duration, Instant},
};

use ast_grep_core::tree_sitter::LanguageExt;
use omp_ast::SupportLang;
use sha2::{Digest, Sha256};
use tree_sitter::{Language, Parser, Tree, WasmStore, wasmtime::Engine};

const RELEASE: &str = "https://github.com/stencil-hq/wasm-grammars/releases/download/v1";

/// `(language, symbol, file, sha256, bytes)`, copied from
/// can1357/oh-my-pi@88a69582c7 `crates/pi-ast/src/language/wasm_grammars.rs`.
const GRAMMARS: &[(&str, &str, &str, &str, u64)] = &[
	(
		"astro",
		"astro",
		"astro-612c83b88b97ba7c.wasm",
		"612c83b88b97ba7cc53a4808aebf71196442d2aea70955333b114212b771a4d7",
		24622,
	),
	(
		"clojure",
		"clojure",
		"clojure-f63ecd31939b06c3.wasm",
		"f63ecd31939b06c315c49794a628059b6f16de191a17b148a290e5f8faffbc0a",
		97934,
	),
	(
		"cmake",
		"cmake",
		"cmake-38a05d65733bf24f.wasm",
		"38a05d65733bf24f4469b01d644d6b8e26e779fcc40ae562037dd699aee6e103",
		75504,
	),
	(
		"csharp",
		"c_sharp",
		"csharp-f265130d20fe62d3.wasm",
		"f265130d20fe62d3dc0cec756ee5a4159bd9825894a48fc0f7e72c8bd30ef099",
		5328197,
	),
	(
		"dart",
		"dart",
		"dart-52bdbf469a2343a5.wasm",
		"52bdbf469a2343a52385f75dab61097fe9c157e747ddfcc52200c153c4259d4b",
		1269300,
	),
	(
		"dockerfile",
		"dockerfile",
		"dockerfile-6d95cfe08d32ad32.wasm",
		"6d95cfe08d32ad32cd4e3e13e209f03ae59fbd495ad61014e1873fbbbb6c5078",
		48699,
	),
	(
		"elixir",
		"elixir",
		"elixir-1160c3b90a7c1eb7.wasm",
		"1160c3b90a7c1eb7cb8f8e5c3a1960db3219c97c10fa21b1421b79142cc2e556",
		1412268,
	),
	(
		"emacs-lisp",
		"elisp",
		"emacs-lisp-d4fee941a25f6464.wasm",
		"d4fee941a25f646467cebe60317f46eb0f47b617c76fb9a1a60e7e7982b43415",
		52513,
	),
	(
		"erlang",
		"erlang",
		"erlang-bb8809212393e686.wasm",
		"bb8809212393e68687d0f2bc1309c4ca7601fa704a8a96edaffaa34585f1b1b7",
		418478,
	),
	(
		"fortran",
		"fortran",
		"fortran-f72872963184cdb4.wasm",
		"f72872963184cdb4ccb623be3e4e40606066a317be6d5d61cdc929b51bba6d22",
		6591350,
	),
	(
		"graphql",
		"graphql",
		"graphql-fd4081c84308f5db.wasm",
		"fd4081c84308f5db14a4a6e1bea91c08f9c38e5ab47e5a0730c4a39448844d00",
		57563,
	),
	(
		"haskell",
		"haskell",
		"haskell-ddc4978970326bc4.wasm",
		"ddc4978970326bc46369f3dff15027c54dac79adb7aca9ba7b6d3a20ebb1b3e0",
		3805634,
	),
	(
		"hcl",
		"hcl",
		"hcl-1b4b186da007c489.wasm",
		"1b4b186da007c48980f7ec7259b15edb2ea6cb1b8cf7ad01d5c9c39456aa52fb",
		93351,
	),
	(
		"ini",
		"ini",
		"ini-4884a74e0e60d02f.wasm",
		"4884a74e0e60d02fc897b1f780c044e578ff839c9eeb6f4742d2feaef9839ece",
		4710,
	),
	(
		"julia",
		"julia",
		"julia-78d8e1aa3b207c29.wasm",
		"78d8e1aa3b207c290631bbb071ebbe20f91d631e91dd2d78a71358472660978a",
		6223351,
	),
	(
		"just",
		"just",
		"just-989d19dab6b34933.wasm",
		"989d19dab6b349337e0335e8f8bfe39de40f27639dc34c23a0d8c63678a69e68",
		44632,
	),
	(
		"kotlin",
		"kotlin",
		"kotlin-83d1aa9580f91a32.wasm",
		"83d1aa9580f91a32b51a057ad4216166c29e488bf750aa891ae528f7a04dd4bf",
		5772696,
	),
	(
		"lua",
		"lua",
		"lua-83446c583739247d.wasm",
		"83446c583739247d74ae0953a1844b642fc126a5e773b797ecd0f7b5af686c84",
		53205,
	),
	(
		"make",
		"make",
		"make-c2a057ef58739b2f.wasm",
		"c2a057ef58739b2f8025bc3f7d04a62ae3daad8aeb7979b2db6097800950cded",
		147044,
	),
	(
		"nix",
		"nix",
		"nix-10a66e15b62be1a5.wasm",
		"10a66e15b62be1a59cd256abf4b48fdeac07e50c0a8e50ceb64296fcec019930",
		79503,
	),
	(
		"objc",
		"objc",
		"objc-ede2d58688f085d4.wasm",
		"ede2d58688f085d4d36ccdf7f2b50aa61f70ea4f134dd3741c23ad857473a148",
		5317156,
	),
	(
		"ocaml",
		"ocaml",
		"ocaml-c60bd9980a698106.wasm",
		"c60bd9980a698106c09dccf5a089841e7d285721ad8ce9e4fd5a326bf3316d0f",
		5553927,
	),
	(
		"odin",
		"odin",
		"odin-0fa018a7182e62a6.wasm",
		"0fa018a7182e62a6414379018fb1bbe2e46ca39f6f1cd6584a4609481c1f303e",
		2215304,
	),
	(
		"php",
		"php_only",
		"php-cb7397cba44a21ce.wasm",
		"cb7397cba44a21ce6882d8173c597d66e32a257cb1f628ba45cdf23f9d8d7c6a",
		1002409,
	),
	(
		"powershell",
		"powershell",
		"powershell-2a24f946f96e67ac.wasm",
		"2a24f946f96e67ac812fb6c7d647576a033febc0c48f4c91a340b549e7eac73d",
		990743,
	),
	(
		"protobuf",
		"proto",
		"protobuf-0b226f4132ac4493.wasm",
		"0b226f4132ac4493642b8df1fbeb7541c92e36e3fb152064ef662d072135b795",
		73836,
	),
	(
		"r",
		"r",
		"r-908227b85fae8f37.wasm",
		"908227b85fae8f37660f9991110349a2ce6ecde650549c8a72cda800caa5e948",
		484429,
	),
	(
		"ruby",
		"ruby",
		"ruby-e77f88559b47c0fe.wasm",
		"e77f88559b47c0fe2771821c6c160cdf6bbfe17afcd642ab3fdda9aebb70b2f3",
		2106322,
	),
	(
		"scala",
		"scala",
		"scala-f1699072abf3668a.wasm",
		"f1699072abf3668a995ece3950a52c00f9da168a58684fe104e98214b0a74903",
		3922744,
	),
	(
		"solidity",
		"solidity",
		"solidity-ac4d44dc54b7a475.wasm",
		"ac4d44dc54b7a475c2284ae27ab0af932043e2cfd4e200a416b2ac5d40f433d7",
		508585,
	),
	(
		"sql",
		"sql",
		"sql-f9d5638965f6f7ec.wasm",
		"f9d5638965f6f7ecc6b6979b8717f44e756687a1cbe4fef411b84506bdb81451",
		2433442,
	),
	(
		"starlark",
		"starlark",
		"starlark-1f892a95b3d42e69.wasm",
		"1f892a95b3d42e69e71eeb3efdba3cc605a5c8b221b554e250f5418c64c682a6",
		329944,
	),
	(
		"svelte",
		"svelte",
		"svelte-03ab329192177443.wasm",
		"03ab329192177443c91eaf0904b011089cc6b779df1792468afd379c0a6a2512",
		44859,
	),
	(
		"swift",
		"swift",
		"swift-92a6aa39712255e7.wasm",
		"92a6aa39712255e7332d1b06db52bc6d49e9f563215846978c0ca63b6cc38359",
		3726992,
	),
	(
		"tlaplus",
		"tlaplus",
		"tlaplus-2bb6711906017f86.wasm",
		"2bb6711906017f861d3c143553c5077fc167f29d023a606a5dcfc159efb9e5bc",
		4878580,
	),
	(
		"verilog",
		"verilog",
		"verilog-46f723039630ef85.wasm",
		"46f723039630ef85b8a2dd324cba2673207163f760eb435c800bc0f56d78bb0e",
		18135336,
	),
	(
		"vue",
		"vue",
		"vue-a9b2b4573afa0f31.wasm",
		"a9b2b4573afa0f31a878753a30e2cda2030baa26544159448da6ba8c69bfa76d",
		28095,
	),
	(
		"xml",
		"xml",
		"xml-fafa7327c7a09ba7.wasm",
		"fafa7327c7a09ba74612e23543ff4e93ee08b785f0a8c2f8292c03f0b65c4917",
		46968,
	),
	(
		"zig",
		"zig",
		"zig-4dd568b541381b9d.wasm",
		"4dd568b541381b9ddcd0b53eda9956d26fea148b26f0af2ec52b1e8b4de3ed2e",
		691729,
	),
];

fn ms(d: Duration) -> f64 {
	d.as_secs_f64() * 1e3
}

fn rss_kb() -> u64 {
	let statm = fs::read_to_string("/proc/self/statm").unwrap_or_default();
	statm
		.split_whitespace()
		.nth(1)
		.and_then(|pages| pages.parse::<u64>().ok())
		.unwrap_or(0)
		* 4
}

fn hex(bytes: &[u8]) -> String {
	bytes
		.iter()
		.fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
			let _ = write!(out, "{byte:02x}");
			out
		})
}

fn grammar(name: &str) -> (&'static str, &'static str, &'static str) {
	let (language, symbol, file, ..) = *GRAMMARS
		.iter()
		.find(|g| g.0 == name)
		.unwrap_or_else(|| panic!("unknown grammar {name}"));
	(language, symbol, file)
}

fn sample(samples: &Path, language: &str) -> String {
	fs::read_to_string(samples.join(format!("{language}.txt"))).unwrap_or_default()
}

fn native_language(language: &str) -> Language {
	SupportLang::from_str(language)
		.expect("SupportLang alias")
		.get_ts_language()
}

fn wasm_language(engine: &Engine, dir: &Path, symbol: &str, file: &str) -> Language {
	let bytes = fs::read(dir.join(file)).expect("wasm module");
	let mut store = WasmStore::new(engine).expect("wasm store");
	store.load_language(symbol, &bytes).expect("load_language")
}

fn wasm_parser(engine: &Engine, language: &Language) -> Parser {
	let mut parser = Parser::new();
	parser
		.set_wasm_store(WasmStore::new(engine).expect("wasm store"))
		.expect("set_wasm_store");
	parser.set_language(language).expect("wasm set_language");
	parser
}

fn native_parser(language: &Language) -> Parser {
	let mut parser = Parser::new();
	parser.set_language(language).expect("native set_language");
	parser
}

fn parse(parser: &mut Parser, source: &str) -> Tree {
	parser.parse(source, None).expect("parse")
}

fn download(dir: &Path, reps: usize) {
	fs::create_dir_all(dir).expect("grammar dir");
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("tokio runtime");
	for rep in 0..reps {
		for i in 0..GRAMMARS.len() {
			let (language, _, file, sha256, size) = GRAMMARS[(i + rep * 7) % GRAMMARS.len()];
			let client = omp_http::client_builder().build().expect("http client");
			let url = format!("{RELEASE}/{file}.zst");
			let start = Instant::now();
			let zst = runtime
				.block_on(async {
					match client
						.get(&url)
						.send()
						.await
						.and_then(|r| r.error_for_status())
					{
						Ok(response) => response.bytes().await.map_err(|e| e.to_string()),
						Err(error) => Err(error.to_string()),
					}
				})
				.expect("download");
			let http = start.elapsed();
			let start = Instant::now();
			let wasm = zstd::stream::decode_all(&zst[..]).expect("zstd");
			let decode = start.elapsed();
			let start = Instant::now();
			let ok = hex(&Sha256::digest(&wasm)) == sha256 && wasm.len() as u64 == size;
			let verify = start.elapsed();
			let start = Instant::now();
			fs::write(dir.join(file), &wasm).expect("write module");
			let write = start.elapsed();
			println!(
				"GS\tdownload\t{language}\t{rep}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{ok}",
				zst.len(),
				wasm.len(),
				ms(http),
				ms(decode),
				ms(verify),
				ms(write)
			);
		}
	}
}

fn first_native(language: &str, source: &str) -> (f64, Tree) {
	let start = Instant::now();
	let mut parser = native_parser(&native_language(language));
	let tree = parse(&mut parser, source);
	(ms(start.elapsed()), tree)
}

fn first(dir: &Path, samples: &Path, name: &str, rep: &str, native_first: bool) {
	let (language, symbol, file) = grammar(name);
	let source = sample(samples, language);
	let native = native_first.then(|| first_native(language, &source));
	let all = Instant::now();
	let start = Instant::now();
	let engine = Engine::default();
	let engine_ms = ms(start.elapsed());
	let start = Instant::now();
	let bytes = fs::read(dir.join(file)).expect("wasm module");
	let read_ms = ms(start.elapsed());
	let start = Instant::now();
	let mut store = WasmStore::new(&engine).expect("wasm store");
	let store_ms = ms(start.elapsed());
	let start = Instant::now();
	let wasm = store.load_language(symbol, &bytes).expect("load_language");
	let compile_ms = ms(start.elapsed());
	drop(store);
	let start = Instant::now();
	let mut parser = Parser::new();
	parser
		.set_wasm_store(WasmStore::new(&engine).expect("wasm store"))
		.expect("set_wasm_store");
	let parser_store_ms = ms(start.elapsed());
	let start = Instant::now();
	parser.set_language(&wasm).expect("wasm set_language");
	let set_language_ms = ms(start.elapsed());
	let start = Instant::now();
	let wasm_tree = parse(&mut parser, &source);
	let parse_ms = ms(start.elapsed());
	let total_ms = ms(all.elapsed());
	let (native_ms, native_tree) = native.unwrap_or_else(|| first_native(language, &source));
	let (w, n) = (wasm_tree.root_node(), native_tree.root_node());
	let order = if native_first {
		"native-first"
	} else {
		"wasm-first"
	};
	let fields = [
		language.to_owned(),
		rep.to_owned(),
		order.to_owned(),
		bytes.len().to_string(),
		source.len().to_string(),
		format!("{engine_ms:.3}"),
		format!("{read_ms:.3}"),
		format!("{store_ms:.3}"),
		format!("{compile_ms:.3}"),
		format!("{parser_store_ms:.3}"),
		format!("{set_language_ms:.3}"),
		format!("{parse_ms:.3}"),
		format!("{total_ms:.3}"),
		format!("{native_ms:.3}"),
		w.descendant_count().to_string(),
		n.descendant_count().to_string(),
		w.has_error().to_string(),
		n.has_error().to_string(),
		(w.to_sexp() == n.to_sexp()).to_string(),
	];
	println!("GS\tfirst\t{}", fields.join("\t"));
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
	sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn steady(dir: &Path, samples: &Path, name: &str, iters: usize) {
	let (language, symbol, file) = grammar(name);
	let source = sample(samples, language);
	let engine = Engine::default();
	let wasm = wasm_language(&engine, dir, symbol, file);
	let native = native_language(language);
	let mut native_p = native_parser(&native);
	let mut wasm_p = wasm_parser(&engine, &wasm);
	let (n_tree, w_tree) = (parse(&mut native_p, &source), parse(&mut wasm_p, &source));
	let equal = n_tree.root_node().to_sexp() == w_tree.root_node().to_sexp();
	let nodes = n_tree.root_node().descendant_count();
	let blocks = 5;
	let per_block = iters.div_ceil(blocks);
	let (mut native_us, mut wasm_us) = (Vec::new(), Vec::new());
	for _ in 0..blocks {
		for _ in 0..per_block {
			let start = Instant::now();
			drop(parse(&mut native_p, &source));
			native_us.push(start.elapsed().as_secs_f64() * 1e6);
		}
		for _ in 0..per_block {
			let start = Instant::now();
			drop(parse(&mut wasm_p, &source));
			wasm_us.push(start.elapsed().as_secs_f64() * 1e6);
		}
	}
	drop((native_p, wasm_p));
	native_us.sort_by(f64::total_cmp);
	wasm_us.sort_by(f64::total_cmp);
	let mut fresh = wasm_parser(&engine, &wasm);
	let rss_wasm_start = rss_kb();
	for _ in 0..blocks * per_block {
		drop(parse(&mut fresh, &source));
	}
	let rss_wasm_end = rss_kb();
	drop(fresh);
	let mut fresh = native_parser(&native);
	let rss_native_start = rss_kb();
	for _ in 0..blocks * per_block {
		drop(parse(&mut fresh, &source));
	}
	let rss_native_end = rss_kb();
	let (n_med, w_med) = (quantile(&native_us, 0.5), quantile(&wasm_us, 0.5));
	let fields = [
		language.to_owned(),
		source.len().to_string(),
		native_us.len().to_string(),
		nodes.to_string(),
		equal.to_string(),
		format!("{n_med:.1}"),
		format!("{:.1}", quantile(&native_us, 0.1)),
		format!("{:.1}", quantile(&native_us, 0.9)),
		format!("{w_med:.1}"),
		format!("{:.1}", quantile(&wasm_us, 0.1)),
		format!("{:.1}", quantile(&wasm_us, 0.9)),
		format!("{:.3}", w_med / n_med),
		rss_wasm_start.to_string(),
		rss_wasm_end.to_string(),
		rss_native_start.to_string(),
		rss_native_end.to_string(),
	];
	println!("GS\tsteady\t{}", fields.join("\t"));
}

fn main() {
	let args: Vec<String> = env::args().collect();
	let arg = |i: usize| args.get(i).map_or("", String::as_str);
	match arg(1) {
		"list" => GRAMMARS.iter().for_each(|g| println!("{}", g.0)),
		"download" => download(Path::new(arg(2)), arg(3).parse().expect("reps")),
		"first" => {
			first(Path::new(arg(2)), Path::new(arg(3)), arg(4), arg(5), arg(6) == "native-first")
		},
		"steady" => {
			steady(Path::new(arg(2)), Path::new(arg(3)), arg(4), arg(5).parse().expect("iters"))
		},
		_ => panic!(
			"usage: grammar_probe list | download DIR REPS | first DIR SAMPLES LANG REP \
			 [native-first] | steady DIR SAMPLES LANG ITERS"
		),
	}
}
