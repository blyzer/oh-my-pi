//! Measures the model-visible tokens the minimizer would save on a corpus of
//! captured command output, without wiring it into any output path.
//!
//! ```text
//! cargo run -p omp-minimizer --example savings --release -- <corpus> <name>=<tokenizer.json>... [--show=<id>,...]
//! ```
//!
//! `<corpus>/manifest.tsv` holds one tab-separated row per capture: id, exit
//! code, source, working-directory label and command. The capture itself is
//! `<corpus>/<id>.out`. The Markdown report goes to stdout; `--show` appends
//! the text the model would see with the minimizer for the named captures.

use std::{
	collections::{BTreeMap, BTreeSet},
	env, error,
	fmt::Write as _,
	fs,
	path::Path,
};

use omp_core::Hash32;
use omp_minimizer::{
	MinimizerConfig, detect, engine::MIN_MINIMIZE_CHARS, primitives::WHOLE_OUTPUT_BYTES,
};
use regex::Regex;
use strum::IntoStaticStr;
use tokenizers::Tokenizer;

type Error = Box<dyn error::Error + Send + Sync>;

/// `LIVE_OUTPUT_BYTES` (crates/envd/src/exec.rs): envd projects this much of
/// a bash command and spills the rest.
const LIVE_OUTPUT_BYTES: usize = 64 * 1024;
/// The production dispatch policy (crates/driver/src/headless/kernel.rs) at
/// the `sv_tools_output_spill_bytes`, `sv_tools_output_max_columns` and
/// `sv_tools_artifact_*` defaults.
const INLINE_BYTES: usize = 50 * 1024;
const LINE_BYTES: usize = 768;
const HEAD_BYTES: usize = 20 * 1024;
const TAIL_BYTES: usize = 20 * 1024;
const PROJECTION_LINES: usize = 500;
/// `BYTES_PER_TOKEN` (crates/agent/src/context.rs).
const BYTES_PER_TOKEN: usize = 4;
/// Failure lines: compiler and lint errors, test failures, panics and format
/// diffs.
const FAILURE: &str = r"(?:^|\s)error(?:\[[A-Z]\d{4}\])?:|\bFAIL(?:ED)?\b|panicked at|^Diff in ";
const FAILURE_KEY_BYTES: usize = 80;

#[derive(Clone, Copy, Debug, Eq, IntoStaticStr, Ord, PartialEq, PartialOrd)]
enum Band {
	#[strum(serialize = "under 1,000 chars")]
	Floor,
	#[strum(serialize = "1,000 chars to 16 KiB")]
	Small,
	#[strum(serialize = "16 KiB to 50 KiB")]
	Inline,
	#[strum(serialize = "50 KiB to 64 KiB")]
	Projected,
	#[strum(serialize = "over 64 KiB")]
	Spilled,
}

impl Band {
	fn of(text: &str) -> Self {
		if text.chars().take(MIN_MINIMIZE_CHARS).count() < MIN_MINIMIZE_CHARS {
			Self::Floor
		} else if text.len() <= WHOLE_OUTPUT_BYTES {
			Self::Small
		} else if text.len() <= INLINE_BYTES {
			Self::Inline
		} else if text.len() <= LIVE_OUTPUT_BYTES {
			Self::Projected
		} else {
			Self::Spilled
		}
	}
}

struct Capture {
	id:      String,
	exit:    i32,
	source:  String,
	command: String,
	text:    String,
}

struct Counter {
	name:      String,
	tokenizer: Option<Tokenizer>,
}

impl Counter {
	fn count(&self, text: &str) -> Result<usize, Error> {
		match &self.tokenizer {
			Some(tokenizer) => Ok(tokenizer.encode_fast(text, false)?.len()),
			None => Ok(text.len().div_ceil(BYTES_PER_TOKEN)),
		}
	}
}

#[derive(Clone, Copy, Default)]
struct Tokens {
	raw:       usize,
	today:     usize,
	minimized: usize,
}

struct Measured<'c> {
	capture:       &'c Capture,
	program:       String,
	filter:        &'static str,
	rewritten:     bool,
	rewrite_bytes: usize,
	band:          Band,
	tokens:        Vec<Tokens>,
	failures:      Option<[usize; 3]>,
	shown:         Option<String>,
}

fn main() -> Result<(), Error> {
	let mut args = env::args().skip(1);
	let corpus = args
		.next()
		.ok_or("usage: savings <corpus> <name>=<tokenizer.json>... [--show=<id>,...]")?;
	let mut show = BTreeSet::new();
	let mut counters = Vec::new();
	for arg in args {
		if let Some(ids) = arg.strip_prefix("--show=") {
			show.extend(ids.split(',').map(str::to_owned));
			continue;
		}
		let (name, path) = arg
			.split_once('=')
			.ok_or_else(|| format!("tokenizer {arg} is not <name>=<path>"))?;
		counters.push(Counter { name: name.to_owned(), tokenizer: Some(Tokenizer::from_file(path)?) });
	}
	counters.push(Counter { name: String::from("bytes/4"), tokenizer: None });

	let captures = load(Path::new(&corpus))?;
	let config = MinimizerConfig { enabled: true, ..MinimizerConfig::default() };
	let failure = Regex::new(FAILURE)?;
	let mut measured = Vec::with_capacity(captures.len());
	for capture in &captures {
		let output = omp_minimizer::apply(&capture.command, &capture.text, capture.exit, &config);
		let digest = Hash32::sum(capture.text.as_bytes()).to_hex();
		let today = today(&capture.text, digest.as_str());
		let minimized = if output.changed {
			minimized(&output.text, digest.as_str(), capture.text.len())
		} else {
			today.clone()
		};
		let mut tokens = Vec::with_capacity(counters.len());
		for counter in &counters {
			let today_tokens = counter.count(&today)?;
			tokens.push(Tokens {
				raw:       counter.count(&capture.text)?,
				today:     today_tokens,
				minimized: if output.changed {
					counter.count(&minimized)?
				} else {
					today_tokens
				},
			});
		}
		let failures = (capture.exit != 0).then(|| {
			let keys = failure_keys(&failure, &capture.text);
			let kept = |visible: &str| keys.iter().filter(|key| visible.contains(**key)).count();
			[keys.len(), kept(&today), kept(&minimized)]
		});
		measured.push(Measured {
			capture,
			program: detect::detect(&capture.command)
				.map_or_else(|| String::from("?"), |identity| identity.program),
			filter: output.filter,
			rewritten: output.changed,
			rewrite_bytes: output.text.len(),
			band: Band::of(&capture.text),
			tokens,
			failures,
			shown: show.contains(&capture.id).then_some(minimized),
		});
	}
	measured.sort_by(|a, b| {
		(a.band, a.capture.text.len(), &a.capture.id).cmp(&(b.band, b.capture.text.len(), &b.capture.id))
	});
	print!("{}", report(&counters, &measured));
	Ok(())
}

fn load(corpus: &Path) -> Result<Vec<Capture>, Error> {
	let manifest = fs::read_to_string(corpus.join("manifest.tsv"))?;
	manifest
		.lines()
		.filter(|line| !line.is_empty())
		.map(|line| {
			let mut fields = line.splitn(5, '\t');
			let mut field = || fields.next().ok_or_else(|| format!("malformed manifest row: {line}"));
			let id = field()?;
			let exit = field()?.parse()?;
			let source = field()?;
			field()?;
			let command = field()?;
			let bytes = fs::read(corpus.join(format!("{id}.out")))?;
			Ok(Capture {
				id: id.to_owned(),
				exit,
				source: source.to_owned(),
				command: command.to_owned(),
				text: String::from_utf8_lossy(&bytes).into_owned(),
			})
		})
		.collect()
}

fn today(capture: &str, digest: &str) -> String {
	if capture.len() <= LIVE_OUTPUT_BYTES {
		return dispatch(capture);
	}
	let mut transcript =
		String::from_utf8_lossy(&capture.as_bytes()[..LIVE_OUTPUT_BYTES]).into_owned();
	push_reference(&mut transcript, digest, capture.len());
	dispatch(&transcript)
}

fn minimized(rewrite: &str, digest: &str, capture_bytes: usize) -> String {
	let mut text = String::with_capacity(rewrite.len() + 128);
	text.push_str(rewrite);
	push_reference(&mut text, digest, capture_bytes);
	dispatch(&text)
}

/// `push_spilled_output` (crates/tools/src/shell.rs).
fn push_reference(text: &mut String, digest: &str, bytes: usize) {
	let _ = writeln!(text, "[full output: artifact://sha256/{digest}; {bytes} bytes]");
}

/// `bound_parts` and `clamp_text` (crates/agent/src/dispatch.rs).
fn dispatch(text: &str) -> String {
	let mut clamped = String::with_capacity(text.len());
	for line in text.split_inclusive('\n') {
		let (body, newline) = line
			.strip_suffix('\n')
			.map_or((line, ""), |body| (body, "\n"));
		if body.len() <= LINE_BYTES {
			clamped.push_str(body);
		} else {
			clamped.push_str(prefix(body, LINE_BYTES));
			clamped.push('…');
		}
		clamped.push_str(newline);
	}
	if clamped.len() <= INLINE_BYTES {
		clamped
	} else {
		projection(&clamped)
	}
}

/// `artifact_projection` (crates/agent/src/dispatch.rs).
fn projection(text: &str) -> String {
	let head_line_end = text
		.match_indices('\n')
		.nth(PROJECTION_LINES - 1)
		.map_or(text.len(), |(index, _)| index + 1);
	let head = prefix(text, HEAD_BYTES.min(head_line_end));
	let mut tail_line_start = 0;
	let mut remaining = PROJECTION_LINES;
	for (index, _) in text.match_indices('\n').rev() {
		if index + 1 == text.len() {
			continue;
		}
		if remaining == 1 {
			tail_line_start = index + 1;
			break;
		}
		remaining -= 1;
	}
	let mut tail_byte_start = text.len().saturating_sub(TAIL_BYTES);
	while !text.is_char_boundary(tail_byte_start) {
		tail_byte_start += 1;
	}
	let tail_start = tail_byte_start.max(tail_line_start);
	if head.len() >= tail_start {
		return text.to_owned();
	}
	let tail = &text[tail_start..];
	let mut output = String::with_capacity(head.len() + tail.len() + 5);
	if !head.is_empty() {
		output.push_str(head);
		if !head.ends_with('\n') {
			output.push('\n');
		}
	}
	output.push('…');
	if !tail.is_empty() {
		output.push('\n');
		output.push_str(tail);
	}
	output
}

fn prefix(text: &str, maximum: usize) -> &str {
	if text.len() <= maximum {
		return text;
	}
	let mut end = maximum;
	while !text.is_char_boundary(end) {
		end -= 1;
	}
	&text[..end]
}

fn failure_keys<'t>(failure: &Regex, text: &'t str) -> BTreeSet<&'t str> {
	text
		.lines()
		.filter_map(|line| {
			failure
				.find(line)
				.map(|found| prefix(line[found.start()..].trim(), FAILURE_KEY_BYTES))
		})
		.collect()
}

fn saved(tokens: Tokens) -> f64 {
	if tokens.today == 0 {
		return 0.0;
	}
	(tokens.today as f64 - tokens.minimized as f64) * 100.0 / tokens.today as f64
}

fn failure_cell(failures: Option<[usize; 3]>) -> String {
	failures.map_or_else(|| String::from("—"), |[of, today, minimized]| {
		format!("{of} / {today} / {minimized}")
	})
}

fn report(counters: &[Counter], measured: &[Measured<'_>]) -> String {
	let mut out = String::new();
	let primary = &counters[0].name;
	let _ = writeln!(out, "Tokens are `{primary}` unless a table names another counter.\n");

	let _ = writeln!(out, "## Per capture\n");
	let _ = writeln!(
		out,
		"| Capture | Command | Exit | Source | Band | Filter | Raw bytes | Rewrite bytes | Raw tok | \
		 Today tok | Minimized tok | Saved | Failure lines (of / today / minimized) |"
	);
	let _ = writeln!(out, "|---|---|--:|---|---|---|--:|--:|--:|--:|--:|--:|---|");
	for row in measured {
		let tokens = row.tokens[0];
		let band: &str = row.band.into();
		let filter = if row.rewritten {
			String::from(row.filter)
		} else {
			format!("{} (unchanged)", row.filter)
		};
		let _ = writeln!(
			out,
			"| {} | `{}` | {} | {} | {band} | {filter} | {} | {} | {} | {} | {} | {:.1}% | {} |",
			row.capture.id,
			row.capture.command.replace('|', "\\|"),
			row.capture.exit,
			row.capture.source,
			row.capture.text.len(),
			row.rewrite_bytes,
			tokens.raw,
			tokens.today,
			tokens.minimized,
			saved(tokens),
			failure_cell(row.failures),
		);
	}

	let _ = writeln!(out, "\n## By size band\n");
	group(&mut out, "Band", measured, |row| String::from(<&str>::from(row.band)));
	let _ = writeln!(out, "\n## By program\n");
	group(&mut out, "Program", measured, |row| row.program.clone());

	let _ = writeln!(out, "\n## By counter\n");
	let _ = writeln!(
		out,
		"| Counter | Today tok, all | Minimized tok, all | Saved, all | Today tok, ≤ 50 KiB | \
		 Minimized tok, ≤ 50 KiB | Saved, ≤ 50 KiB |"
	);
	let _ = writeln!(out, "|---|--:|--:|--:|--:|--:|--:|");
	for (index, counter) in counters.iter().enumerate() {
		let all = total(measured.iter(), index);
		let inline = total(
			measured
				.iter()
				.filter(|row| row.capture.text.len() <= INLINE_BYTES),
			index,
		);
		let _ = writeln!(
			out,
			"| {} | {} | {} | {:.1}% | {} | {} | {:.1}% |",
			counter.name,
			all.today,
			all.minimized,
			saved(all),
			inline.today,
			inline.minimized,
			saved(inline),
		);
	}

	let _ = writeln!(out, "\n## Failure lines in failing captures\n");
	let _ = writeln!(
		out,
		"| Capture | Command | Exit | Filter | Failure lines | Visible today | Visible minimized |"
	);
	let _ = writeln!(out, "|---|---|--:|---|--:|--:|--:|");
	for row in measured {
		let Some([of, today, minimized]) = row.failures else {
			continue;
		};
		let _ = writeln!(
			out,
			"| {} | `{}` | {} | {} | {of} | {today} | {minimized} |",
			row.capture.id,
			row.capture.command.replace('|', "\\|"),
			row.capture.exit,
			row.filter,
		);
	}

	for row in measured {
		let Some(shown) = &row.shown else {
			continue;
		};
		let _ = writeln!(out, "\n## With the minimizer: {}\n\n`{}`\n", row.capture.id, row.capture.command);
		let _ = writeln!(out, "````text\n{shown}````");
	}
	out
}

fn total<'r, 'c: 'r>(rows: impl Iterator<Item = &'r Measured<'c>>, counter: usize) -> Tokens {
	rows.fold(Tokens::default(), |sum, row| Tokens {
		raw:       sum.raw + row.tokens[counter].raw,
		today:     sum.today + row.tokens[counter].today,
		minimized: sum.minimized + row.tokens[counter].minimized,
	})
}

fn group(
	out: &mut String,
	label: &str,
	measured: &[Measured<'_>],
	key: impl Fn(&Measured<'_>) -> String,
) {
	let mut groups = BTreeMap::<String, Vec<&Measured<'_>>>::new();
	for row in measured {
		groups.entry(key(row)).or_default().push(row);
	}
	let _ = writeln!(
		out,
		"| {label} | Captures | Rewritten | Raw tok | Today tok | Minimized tok | Saved | \
		 Per-capture saved: min / median / max |"
	);
	let _ = writeln!(out, "|---|--:|--:|--:|--:|--:|--:|---|");
	for (name, rows) in groups {
		let sum = total(rows.iter().copied(), 0);
		let mut each = rows.iter().map(|row| saved(row.tokens[0])).collect::<Vec<_>>();
		each.sort_by(f64::total_cmp);
		let _ = writeln!(
			out,
			"| {name} | {} | {} | {} | {} | {} | {:.1}% | {:.1}% / {:.1}% / {:.1}% |",
			rows.len(),
			rows.iter().filter(|row| row.rewritten).count(),
			sum.raw,
			sum.today,
			sum.minimized,
			saved(sum),
			each[0],
			each[(each.len() - 1) / 2],
			each[each.len() - 1],
		);
	}
}
