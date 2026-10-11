//! Every builtin that reads a file's contents does so through the installed
//! path policy (`Host::open_read`), so a `read_deny` root hides the file from
//! all of them: each run on a hidden file fails as the builtin fails on an
//! unreadable one, and none of them prints a byte of it. The same file outside
//! the hidden root reads normally, so each refusal is the policy's.

use std::{fs, path::PathBuf, sync::Arc};

use crate::{
	b2sum::B2sum,
	base32::Base32,
	cat::Cat,
	cksum::Cksum,
	cmp::Cmp,
	combine::Combine,
	comm::Comm,
	cut::Cut,
	date::Date,
	diff::Diff,
	grep::Grep,
	head::Head,
	host::{Capture, HiddenRoot, Utility, run_util, run_util_with_policy},
	isutf8::Isutf8,
	jq::Jq,
	paste::Paste,
	rg::Rg,
	sed::Sed,
	sort::Sort,
	tac::Tac,
	tail::Tail,
	uniq::Uniq,
	wc::Wc,
	xargs::Xargs,
};

/// What the hidden file holds; no refused read may print it.
const SECRET: &str = "omp-secret-marker";

/// A workspace holding `hidden/secret` and identical admitted copies `open`
/// and `mirror/secret`.
struct Fixture {
	_dir: tempfile::TempDir,
	root: PathBuf,
}

impl Fixture {
	fn new() -> Self {
		let dir = tempfile::tempdir().expect("workspace");
		let root = dir.path().canonicalize().expect("canonical workspace");
		fs::create_dir(root.join("hidden")).expect("hidden root");
		fs::create_dir(root.join("mirror")).expect("admitted mirror");
		for name in ["hidden/secret", "mirror/secret", "open"] {
			fs::write(root.join(name), format!("{SECRET}\n")).expect("file");
		}
		fs::write(root.join("hidden/secret.json"), format!("\"{SECRET}\"\n")).expect("json");
		Self { _dir: dir, root }
	}

	fn policy(&self) -> Arc<HiddenRoot> {
		Arc::new(HiddenRoot(self.root.join("hidden")))
	}

	/// Runs `U` in the workspace under the hidden-root policy.
	fn run<U: Utility>(&self, argv: &[&str]) -> (i32, Capture) {
		run_util_with_policy::<U>(argv, "", &self.root, self.policy())
	}
}

/// Asserts `U` refuses `argv`, which names a hidden file: a non-zero exit, the
/// policy's denial on stderr, and no secret on either stream.
fn refused<U: Utility>(fixture: &Fixture, argv: &[&str]) {
	let (code, capture) = fixture.run::<U>(argv);
	let name = U::NAME;
	assert_ne!(code, 0, "{name} {argv:?} read a hidden file: {}", capture.out());
	assert!(!capture.out().contains(SECRET), "{name} {argv:?} printed the secret");
	assert!(!capture.err().contains(SECRET), "{name} {argv:?} leaked the secret");
	assert!(
		capture.err().contains("sandbox denied read"),
		"{name} {argv:?} reports the denial: {}",
		capture.err()
	);
}

/// Asserts `U` reads the admitted `argv` and prints the content.
fn admitted_reads<U: Utility>(fixture: &Fixture, argv: &[&str]) {
	let (code, capture) = fixture.run::<U>(argv);
	assert_eq!(code, 0, "{} {argv:?}: {}", U::NAME, capture.err());
	assert!(capture.out().contains(SECRET), "{} {argv:?}: {}", U::NAME, capture.out());
}

/// The three builtins the acceptance names: `cat`, `head` and `grep` refuse
/// a hidden path, by relative and absolute spelling, and read the admitted
/// twin.
#[test]
fn cat_head_and_grep_refuse_hidden_paths() {
	let fixture = Fixture::new();
	let absolute = fixture.root.join("hidden/secret");
	let absolute = absolute.to_str().expect("UTF-8 workspace");
	for path in ["hidden/secret", absolute] {
		refused::<Cat>(&fixture, &[path]);
		refused::<Head>(&fixture, &[path]);
		refused::<Grep>(&fixture, &["omp", path]);
	}
	// Recursive grep meets the hidden file through the walk.
	refused::<Grep>(&fixture, &["-r", "omp", "hidden"]);
	admitted_reads::<Cat>(&fixture, &["open"]);
	admitted_reads::<Head>(&fixture, &["open"]);
	admitted_reads::<Grep>(&fixture, &["omp", "open"]);

	// `cat a hidden b` reports the hidden one and keeps going, as for any
	// unreadable operand.
	let (code, capture) = fixture.run::<Cat>(&["open", "hidden/secret", "open"]);
	assert_eq!(code, 1);
	assert_eq!(capture.out(), format!("{SECRET}\n{SECRET}\n"));
}

/// Every other builtin that reads file contents refuses the hidden file too.
#[test]
fn every_reading_builtin_refuses_hidden_paths() {
	let fixture = Fixture::new();
	let secret = "hidden/secret";
	refused::<B2sum>(&fixture, &[secret]);
	refused::<Base32>(&fixture, &[secret]);
	refused::<Cksum>(&fixture, &[secret]);
	refused::<Cmp>(&fixture, &[secret, "open"]);
	refused::<Combine>(&fixture, &[secret, "and", "open"]);
	refused::<Comm>(&fixture, &[secret, "open"]);
	refused::<Cut>(&fixture, &["-c1-", secret]);
	refused::<Date>(&fixture, &["-f", secret]);
	refused::<Diff>(&fixture, &[secret, "open"]);
	refused::<Diff>(&fixture, &["-r", "hidden", "mirror"]);
	refused::<Grep>(&fixture, &["-f", secret, "open"]);
	refused::<Isutf8>(&fixture, &[secret]);
	refused::<Jq>(&fixture, &[".", "hidden/secret.json"]);
	refused::<Jq>(&fixture, &["--rawfile", "s", secret, "-n", "$s"]);
	refused::<Paste>(&fixture, &[secret]);
	refused::<Rg>(&fixture, &["omp", secret]);
	refused::<Rg>(&fixture, &["omp", "hidden"]);
	refused::<Sed>(&fixture, &["", secret]);
	refused::<Sed>(&fixture, &["-f", secret, "open"]);
	refused::<Sort>(&fixture, &[secret]);
	refused::<Sort>(&fixture, &["-m", secret, "open"]);
	refused::<Sort>(&fixture, &["-c", secret]);
	refused::<Tac>(&fixture, &[secret]);
	refused::<Tail>(&fixture, &[secret]);
	refused::<Uniq>(&fixture, &[secret]);
	refused::<Wc>(&fixture, &[secret]);
	refused::<Xargs>(&fixture, &["-a", secret, "echo"]);

	admitted_reads::<Sort>(&fixture, &["open"]);
	admitted_reads::<Tail>(&fixture, &["open"]);
	admitted_reads::<Jq>(&fixture, &["-r", "--rawfile", "s", "open", "-n", "$s"]);
	admitted_reads::<Rg>(&fixture, &["omp", "open"]);
}

/// A script-named `r` file is read through the policy: sed treats an
/// unreadable `r` file as empty (POSIX), so the hidden one adds nothing.
#[test]
fn sed_r_files_honour_the_read_policy() {
	let fixture = Fixture::new();
	let (code, capture) = fixture.run::<Sed>(&["r hidden/secret", "open"]);
	assert_eq!(code, 0, "{}", capture.err());
	assert_eq!(capture.out(), format!("{SECRET}\n"), "only the admitted input line");
	let (code, capture) = run_util::<Sed>(&["r hidden/secret", "open"], "", &fixture.root);
	assert_eq!(code, 0, "{}", capture.err());
	assert_eq!(capture.out(), format!("{SECRET}\n{SECRET}\n"), "no policy, the r file reads");
}

/// A missing file under a policy is the ordinary missing file, never a
/// sandbox denial.
#[test]
fn missing_files_stay_missing_under_a_policy() {
	let fixture = Fixture::new();
	let (code, capture) = fixture.run::<Cat>(&["absent"]);
	assert_eq!(code, 1);
	assert!(capture.err().contains("No such file or directory"), "{}", capture.err());
	assert!(!capture.err().contains("sandbox denied"), "{}", capture.err());
}
