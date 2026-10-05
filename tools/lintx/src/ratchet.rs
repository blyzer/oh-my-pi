//! Per-crate count ratchet for the error-formatting rules.
//!
//! `error-str-payload` and `error-format` (see [`crate::lints`]) describe
//! patterns that `AGENTS.md` rejects but that hundreds of existing sites still
//! use. Rather than report them all, the ratchet counts findings per crate and
//! compares each count with a committed baseline file:
//!
//! - `--ratchet <baseline>` fails (exit 1) when any crate's count for a rule is
//!   above its baseline, listing that crate's findings. Crates and rules absent
//!   from the baseline have a baseline of zero.
//! - `--ratchet-update <baseline>` rewrites the baseline with lowered counts
//!   (crates that reach zero are dropped). It refuses, writing nothing, when
//!   any count is above the baseline, so the file can only go down. A missing
//!   file is created from the current counts: that is the one-time bootstrap.
//!
//! Baseline format: `[rule]` sections of `crate = count` lines; `#` comments.

use std::{
	collections::BTreeMap,
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use walkdir::WalkDir;

use crate::{
	lint::{AnyLint, FileContext},
	lints,
};

/// Findings of one file, rendered, keyed for reporting.
pub struct Site {
	rule:     &'static str,
	krate:    String,
	rendered: String,
}

/// Counts and sites for a whole scan.
#[derive(Default)]
pub struct Scan {
	/// `rule -> crate -> count`.
	pub counts: Counts,
	sites:      Vec<Site>,
}

/// `rule -> crate -> count`; zero counts are never stored.
pub type Counts = BTreeMap<String, BTreeMap<String, usize>>;

/// What `--ratchet` / `--ratchet-update` do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
	/// Fail when a count rose above the baseline.
	Check,
	/// Lower the baseline; refuse to raise it.
	Update,
}

/// Runs the ratchet over `paths`; returns the process exit code.
pub fn run(mode: Mode, baseline: &Path, paths: &[PathBuf]) -> i32 {
	let scan = scan(paths, &lints::ratchet());
	let existing = match fs::read_to_string(baseline) {
		Ok(text) => match parse(&text) {
			Ok(counts) => Some(counts),
			Err(message) => {
				eprintln!("{}: {message}", baseline.display());
				return 2;
			},
		},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound && mode == Mode::Update => None,
		Err(error) => {
			eprintln!("{}: cannot read baseline: {error}", baseline.display());
			return 2;
		},
	};
	let empty = Counts::new();
	let regressions = regressions(existing.as_ref().unwrap_or(&empty), &scan.counts);
	if existing.is_some() || mode == Mode::Check {
		report_regressions(&scan, &regressions);
	}
	match mode {
		Mode::Check => {
			print_summary(existing.as_ref().unwrap_or(&empty), &scan.counts);
			i32::from(!regressions.is_empty())
		},
		Mode::Update => {
			if existing.is_some() && !regressions.is_empty() {
				eprintln!(
					"refusing to update {}: counts above the baseline cannot be recorded; fix the new \
					 sites (or mark a genuine boundary with `// lintx-allow: <rule> <reason>`)",
					baseline.display()
				);
				return 1;
			}
			let next = existing
				.as_ref()
				.map_or_else(|| scan.counts.clone(), |old| lowered(old, &scan.counts));
			if existing.as_ref() == Some(&next) {
				eprintln!("{} already matches the tree; nothing to lower", baseline.display());
				return 0;
			}
			if let Some(parent) = baseline.parent() {
				let _ = fs::create_dir_all(parent);
			}
			if let Err(error) = fs::write(baseline, render(&next)) {
				eprintln!("{}: cannot write baseline: {error}", baseline.display());
				return 2;
			}
			eprintln!("wrote {}", baseline.display());
			print_summary(&next, &scan.counts);
			0
		},
	}
}

/// Counts every ratchet finding under `paths`.
pub fn scan(paths: &[PathBuf], rules: &[Box<dyn AnyLint>]) -> Scan {
	let mut scan = Scan::default();
	for root in paths {
		for entry in WalkDir::new(root)
			.into_iter()
			.filter_entry(|entry| {
				let name = entry.file_name().to_string_lossy();
				name != "target" && name != "vendor" && !name.starts_with('.')
			})
			.filter_map(Result::ok)
			.filter(|entry| entry.file_type().is_file())
		{
			let path = entry.path();
			if !counts_toward_ratchet(path) {
				continue;
			}
			let Ok(text) = fs::read_to_string(path) else {
				continue;
			};
			let krate = crate_key(path);
			let ctx = FileContext::new(path, &text);
			for rule in rules {
				rule.detect_erased(&ctx, &mut |diag| {
					*scan
						.counts
						.entry(diag.rule.to_owned())
						.or_default()
						.entry(krate.clone())
						.or_default() += 1;
					scan.sites.push(Site {
						rule:     diag.rule,
						krate:    krate.clone(),
						rendered: diag.render(&ctx),
					});
				});
			}
		}
	}
	scan
}

/// Library and binary sources only: test trees, benches, examples, build
/// scripts and test-module files are outside the rule's scope.
fn counts_toward_ratchet(path: &Path) -> bool {
	if path.extension().is_none_or(|extension| extension != "rs") {
		return false;
	}
	let skipped_dir = path.components().any(|component| {
		matches!(component.as_os_str().to_str(), Some("tests" | "benches" | "examples"))
	});
	let name = path
		.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or("");
	!skipped_dir
		&& name != "build.rs"
		&& name != "tests.rs"
		&& !name.ends_with("_tests.rs")
		&& !name.ends_with("_test.rs")
}

/// Directory name under `crates/` that owns `path` (`crates/driver/src/x.rs`
/// is `driver`); `.` when the path is not under a `crates` directory.
pub fn crate_key(path: &Path) -> String {
	let mut components = path.components().map(|c| c.as_os_str().to_string_lossy());
	while let Some(component) = components.next() {
		if component == "crates" {
			return components
				.next()
				.map_or_else(|| ".".into(), |c| c.into_owned());
		}
	}
	".".into()
}

/// `(rule, crate, current, baseline)` for every count above its baseline.
pub fn regressions(baseline: &Counts, current: &Counts) -> Vec<(String, String, usize, usize)> {
	let mut out = Vec::new();
	for (rule, crates) in current {
		for (krate, &count) in crates {
			let allowed = baseline
				.get(rule)
				.and_then(|crates| crates.get(krate))
				.copied()
				.unwrap_or(0);
			if count > allowed {
				out.push((rule.clone(), krate.clone(), count, allowed));
			}
		}
	}
	out
}

/// Baseline with every count lowered to the current one; never raised. Crates
/// that reach zero are dropped.
pub fn lowered(baseline: &Counts, current: &Counts) -> Counts {
	let mut next = Counts::new();
	for (rule, crates) in baseline {
		for (krate, &old) in crates {
			let now = current
				.get(rule)
				.and_then(|crates| crates.get(krate))
				.copied()
				.unwrap_or(0);
			let kept = old.min(now);
			if kept > 0 {
				next
					.entry(rule.clone())
					.or_default()
					.insert(krate.clone(), kept);
			}
		}
	}
	next
}

fn report_regressions(scan: &Scan, regressions: &[(String, String, usize, usize)]) {
	for (rule, krate, current, allowed) in regressions {
		for site in scan
			.sites
			.iter()
			.filter(|site| site.rule == rule && &site.krate == krate)
		{
			println!("{}", site.rendered);
		}
		eprintln!(
			"ratchet: [{rule}] crate `{krate}` has {current} findings, baseline allows {allowed}"
		);
	}
}

fn print_summary(baseline: &Counts, current: &Counts) {
	let mut rules: Vec<&String> = baseline.keys().chain(current.keys()).collect();
	rules.sort();
	rules.dedup();
	for rule in rules {
		let now: usize = current.get(rule).map_or(0, |crates| crates.values().sum());
		let base: usize = baseline.get(rule).map_or(0, |crates| crates.values().sum());
		eprintln!("== {rule}: {now} found, baseline {base}");
		if now < base {
			eprintln!("   counts are below the baseline; lower it with `just lintx-ratchet-update`");
		}
	}
}

/// Parses a baseline file.
pub fn parse(text: &str) -> Result<Counts, String> {
	let mut counts = Counts::new();
	let mut section: Option<String> = None;
	for (index, raw) in text.lines().enumerate() {
		let line = raw.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		if let Some(name) = line
			.strip_prefix('[')
			.and_then(|rest| rest.strip_suffix(']'))
		{
			section = Some(name.trim().to_owned());
			continue;
		}
		let (Some(rule), Some((krate, value))) = (&section, line.split_once('=')) else {
			return Err(format!("line {}: expected `[rule]` or `crate = count`", index + 1));
		};
		let value: usize = value
			.trim()
			.parse()
			.map_err(|_| format!("line {}: `{}` is not a count", index + 1, value.trim()))?;
		counts
			.entry(rule.clone())
			.or_default()
			.insert(krate.trim().to_owned(), value);
	}
	Ok(counts)
}

/// Serializes counts in the baseline format, with the explanatory header.
pub fn render(counts: &Counts) -> String {
	let mut out = String::from(
		"# Error-formatting ratchet baseline (ADR 0035). Per-crate counts of the \
		 `error-str-payload`\n# and `error-format` lintx rules; see tools/lintx/README.md for the \
		 exact patterns.\n# A crate may never exceed its count; absent crates have a baseline of \
		 zero. Lower this file\n# with `just lintx-ratchet-update` after migrating sites; it \
		 refuses to raise any count.\n",
	);
	for (rule, crates) in counts {
		let _ = write!(out, "\n[{rule}]\n");
		for (krate, count) in crates {
			let _ = writeln!(out, "{krate} = {count}");
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn counts(entries: &[(&str, &str, usize)]) -> Counts {
		let mut out = Counts::new();
		for &(rule, krate, count) in entries {
			out.entry(rule.into())
				.or_default()
				.insert(krate.into(), count);
		}
		out
	}

	#[test]
	fn baseline_round_trips() {
		let original =
			counts(&[("error-format", "ai", 4), ("error-format", "app", 9), ("r", "x", 1)]);
		assert_eq!(parse(&render(&original)).expect("parses"), original);
	}

	#[test]
	fn parse_rejects_malformed_lines() {
		assert!(parse("ai = 3\n").is_err(), "entry before any section");
		assert!(parse("[r]\nai = many\n").is_err());
		assert!(parse("[r]\n# comment\nai = 3 # trailing\n").is_ok());
	}

	#[test]
	fn only_rises_are_regressions() {
		let base = counts(&[("r", "ai", 5), ("r", "app", 2)]);
		let now = counts(&[("r", "ai", 5), ("r", "app", 3), ("r", "new", 1), ("q", "ai", 1)]);
		let mut found = regressions(&base, &now);
		found.sort();
		assert_eq!(found, vec![
			("q".to_owned(), "ai".to_owned(), 1, 0),
			("r".to_owned(), "app".to_owned(), 3, 2),
			("r".to_owned(), "new".to_owned(), 1, 0),
		]);
		assert!(regressions(&base, &counts(&[("r", "ai", 1)])).is_empty());
	}

	#[test]
	fn lowering_never_raises_and_drops_zero_crates() {
		let base = counts(&[("r", "ai", 5), ("r", "app", 2), ("r", "gone", 1)]);
		let now = counts(&[("r", "ai", 3), ("r", "app", 2), ("r", "gone", 0), ("r", "new", 7)]);
		assert_eq!(lowered(&base, &now), counts(&[("r", "ai", 3), ("r", "app", 2)]));
	}

	#[test]
	fn crate_key_reads_the_directory_under_crates() {
		assert_eq!(crate_key(Path::new("crates/driver/src/a/b.rs")), "driver");
		assert_eq!(crate_key(Path::new("/w/crates/ai/src/lib.rs")), "ai");
		assert_eq!(crate_key(Path::new("tools/x.rs")), ".");
	}

	#[test]
	fn test_trees_and_scripts_do_not_count() {
		assert!(counts_toward_ratchet(Path::new("crates/ai/src/lib.rs")));
		assert!(counts_toward_ratchet(Path::new("crates/app/src/main.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/ai/tests/a.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/ai/benches/a.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/ai/examples/a.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/ai/build.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/driver/src/v1_import/tests.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/driver/src/x/settings_tests.rs")));
		assert!(!counts_toward_ratchet(Path::new("crates/ai/src/lib.md")));
	}

	#[test]
	fn update_creates_lowers_and_refuses_to_raise() {
		let dir = std::env::temp_dir().join(format!("lintx-ratchet-{}", std::process::id()));
		let tree = dir.join("crates/demo/src");
		fs::create_dir_all(&tree).expect("mkdir");
		fs::write(
			tree.join("lib.rs"),
			"enum E { #[error(\"a: {0}\")] A(Str), #[error(\"b: {0}\")] B(Str) }\n",
		)
		.expect("write source");
		let baseline = dir.join("baseline.toml");
		let paths = [dir.join("crates")];

		assert_eq!(
			run(Mode::Check, &baseline, &paths),
			2,
			"missing baseline is an error in check mode"
		);
		assert_eq!(run(Mode::Update, &baseline, &paths), 0, "bootstrap creates the file");
		assert_eq!(
			parse(&fs::read_to_string(&baseline).expect("read")).expect("parse"),
			counts(&[("error-str-payload", "demo", 2)])
		);
		assert_eq!(run(Mode::Check, &baseline, &paths), 0);

		fs::write(tree.join("lib.rs"), "enum E { #[error(\"a: {0}\")] A(Str) }\n").expect("write");
		assert_eq!(run(Mode::Check, &baseline, &paths), 0, "falling below the baseline passes");
		assert_eq!(run(Mode::Update, &baseline, &paths), 0);
		assert_eq!(
			parse(&fs::read_to_string(&baseline).expect("read")).expect("parse"),
			counts(&[("error-str-payload", "demo", 1)])
		);

		fs::write(
			tree.join("lib.rs"),
			"enum E { #[error(\"a: {0}\")] A(Str), #[error(\"b: {0}\")] B(Str) }\n",
		)
		.expect("write");
		assert_eq!(run(Mode::Check, &baseline, &paths), 1, "a rise fails the check");
		assert_eq!(run(Mode::Update, &baseline, &paths), 1, "update refuses to raise");
		assert_eq!(
			parse(&fs::read_to_string(&baseline).expect("read")).expect("parse"),
			counts(&[("error-str-payload", "demo", 1)]),
			"refused update leaves the file untouched"
		);
		let _ = fs::remove_dir_all(&dir);
	}
}
