//! Multi-language checker discovery and effect-aware execution.

use std::{
	collections::{BTreeSet, HashSet},
	env,
	error::Error as StdError,
	ffi::OsStr,
	fs,
	future::Future,
	io,
	path::{Path, PathBuf},
};

use futures::{StreamExt as _, stream};
use omp_core::{Str, sf};
use tokio_util::sync::CancellationToken;

use super::{
	parsers::{ParseError, ParserInput, ParserKind, parse},
	types::{CheckLogs, CheckResult, Checker, CheckerEffect, Diagnostic, Report, SkippedCheck},
};

/// Binary resolution authority used during discovery.
pub trait BinaryResolver {
	/// Resolves the first of `names` the admitted system executable set holds.
	fn resolve(&self, names: &[&str]) -> Option<PathBuf>;
}
/// Resolver over the absolute directories of the process `$PATH`.
///
/// Project-local executables (`<root>/<name>`, `node_modules/.bin`,
/// `.venv/bin`, ...) are never candidates: a cloned repository must not be able
/// to shadow `cargo` or a checker by committing an executable of that name.
/// Relative `$PATH` entries (`.`, the empty entry) are skipped for the same
/// reason — they name whatever directory the command happens to run in.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilesystemResolver;

impl BinaryResolver for FilesystemResolver {
	fn resolve(&self, names: &[&str]) -> Option<PathBuf> {
		resolve_on_path(env::var_os("PATH").as_deref(), names)
	}
}

/// Resolves the first of `names` found in an absolute directory of `path`.
fn resolve_on_path(path: Option<&OsStr>, names: &[&str]) -> Option<PathBuf> {
	let path = path?;
	names.iter().find_map(|name| {
		env::split_paths(path)
			.filter(|directory| directory.is_absolute())
			.map(|directory| directory.join(name))
			.find(|candidate| runnable_file(candidate))
	})
}

fn runnable_file(path: &Path) -> bool {
	let Ok(metadata) = path.metadata() else {
		return false;
	};
	if !metadata.is_file() {
		return false;
	}
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt as _;
		metadata.permissions().mode() & 0o111 != 0
	}
	#[cfg(not(unix))]
	{
		true
	}
}

/// Recursively snapshots project files while pruning generated dependency and
/// VCS directories.
pub fn scan_project_files(project_root: &Path) -> Result<Vec<PathBuf>, io::Error> {
	let mut files = Vec::new();
	let mut pending = vec![project_root.to_path_buf()];
	while let Some(directory) = pending.pop() {
		for entry in fs::read_dir(directory)? {
			let entry = entry?;
			let kind = entry.file_type()?;
			if kind.is_dir() {
				let ignored = entry.file_name().to_str().is_some_and(|name| {
					matches!(
						name,
						".git"
							| ".hg" | ".svn"
							| "node_modules"
							| "target" | "dist"
							| "build" | ".venv"
							| "venv" | "__pycache__"
							| ".terraform"
					)
				});
				if !ignored {
					pending.push(entry.path());
				}
			} else if kind.is_file() {
				files.push(entry.path());
			}
		}
	}
	files.sort();
	Ok(files)
}

/// Captured checker process output, complete on return and complete-line-only
/// when delivered through the partial-output channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
	/// Exit status.
	pub exit_code: Option<i32>,
	/// Captured standard output.
	pub stdout:    Str,
	/// Captured standard error.
	pub stderr:    Str,
	/// Both streams, complete, as retained in the project blob store; `None`
	/// for partial output and when the runner retains nothing.
	pub logs:      Option<CheckLogs>,
}

/// Supervised process authority for checker execution.
pub trait CheckerRunner {
	/// Typed process failure.
	type Error: StdError + Send + Sync + 'static;

	/// Executes an argv without shell parsing and optionally publishes
	/// accumulated complete-line output while the process remains active.
	fn run_checker(
		&self,
		checker: &Checker,
		cancel: &CancellationToken,
		partials: Option<flume::Sender<ProcessOutput>>,
	) -> impl Future<Output = Result<ProcessOutput, Self::Error>> + Send;
}

/// Discovery result before picker selection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Suite {
	/// Runnable checkers.
	pub checkers: Vec<Checker>,
	/// Checkers omitted because a required binary was unavailable.
	pub skipped:  Vec<SkippedCheck>,
}

#[derive(Clone, Copy)]
struct Family {
	id:         &'static str,
	label:      &'static str,
	language:   &'static str,
	extensions: &'static [&'static str],
	markers:    &'static [&'static str],
	binaries:   &'static [&'static str],
	args:       &'static [&'static str],
	parser:     ParserKind,
	effect:     CheckerEffect,
	test:       bool,
}

const FAMILIES: &[Family] = &[
	Family {
		id:         "cargo-check",
		label:      "cargo check",
		language:   "Rust",
		extensions: &["rs"],
		markers:    &["Cargo.toml"],
		binaries:   &["cargo"],
		args:       &["check", "--message-format=json"],
		parser:     ParserKind::Rust,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "cargo-clippy",
		label:      "cargo clippy",
		language:   "Rust",
		extensions: &["rs"],
		markers:    &["Cargo.toml"],
		binaries:   &["cargo"],
		args:       &["clippy", "--message-format=json", "--", "-Dwarnings"],
		parser:     ParserKind::Rust,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "cargo-test",
		label:      "cargo test",
		language:   "Rust",
		extensions: &["rs"],
		markers:    &["Cargo.toml"],
		binaries:   &["cargo"],
		args:       &["test", "--message-format=json"],
		parser:     ParserKind::RustTest,
		effect:     CheckerEffect::ReadOnly,
		test:       true,
	},
	Family {
		id:         "go-vet",
		label:      "go vet",
		language:   "Go",
		extensions: &["go"],
		markers:    &["go.mod", "go.work"],
		binaries:   &["go"],
		args:       &["vet", "./..."],
		parser:     ParserKind::Go,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "go-test",
		label:      "go test",
		language:   "Go",
		extensions: &["go"],
		markers:    &["go.mod", "go.work"],
		binaries:   &["go"],
		args:       &["test", "-json", "./..."],
		parser:     ParserKind::GoTest,
		effect:     CheckerEffect::ReadOnly,
		test:       true,
	},
	Family {
		id:         "staticcheck",
		label:      "staticcheck",
		language:   "Go",
		extensions: &["go"],
		markers:    &["go.mod"],
		binaries:   &["staticcheck"],
		args:       &["./..."],
		parser:     ParserKind::Staticcheck,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "golangci-lint",
		label:      "golangci-lint",
		language:   "Go",
		extensions: &["go"],
		markers:    &["go.mod"],
		binaries:   &["golangci-lint"],
		// golangci-lint v2 replaced v1's `--out-format=json` with `--output.json.path`.
		args:       &["run", "--output.json.path=stdout"],
		parser:     ParserKind::Golangci,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "ruff",
		label:      "ruff check",
		language:   "Python",
		extensions: &["py", "pyi"],
		markers:    &["pyproject.toml", "ruff.toml", ".ruff.toml"],
		binaries:   &["ruff"],
		args:       &["check", "--output-format=json", "."],
		parser:     ParserKind::Ruff,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "pyright",
		label:      "pyright",
		language:   "Python",
		extensions: &["py", "pyi"],
		markers:    &["pyproject.toml", "pyrightconfig.json"],
		binaries:   &["pyright"],
		args:       &["--outputjson"],
		parser:     ParserKind::Pyright,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "mypy",
		label:      "mypy",
		language:   "Python",
		extensions: &["py", "pyi"],
		markers:    &["pyproject.toml", "mypy.ini", "setup.cfg"],
		binaries:   &["mypy"],
		args:       &["."],
		parser:     ParserKind::Mypy,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "pylint",
		label:      "pylint",
		language:   "Python",
		extensions: &["py"],
		markers:    &["pyproject.toml", ".pylintrc"],
		binaries:   &["pylint"],
		args:       &["--output-format=json", "."],
		parser:     ParserKind::Pylint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "flake8",
		label:      "flake8",
		language:   "Python",
		extensions: &["py"],
		markers:    &["setup.cfg", ".flake8"],
		binaries:   &["flake8"],
		args:       &["."],
		parser:     ParserKind::Flake8,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "ty",
		label:      "ty check",
		language:   "Python",
		extensions: &["py", "pyi"],
		markers:    &["pyproject.toml"],
		binaries:   &["ty"],
		args:       &["check"],
		parser:     ParserKind::Ty,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "pytest",
		label:      "pytest",
		language:   "Python",
		extensions: &["py"],
		markers:    &["pyproject.toml", "pytest.ini"],
		binaries:   &["pytest"],
		args:       &["-q"],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       true,
	},
	Family {
		id:         "eslint",
		label:      "eslint",
		language:   "JavaScript",
		extensions: &["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"],
		markers:    &["package.json", "eslint.config.js", "eslint.config.mjs", ".eslintrc"],
		binaries:   &["eslint"],
		args:       &[".", "--format=json"],
		parser:     ParserKind::Eslint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "biome",
		label:      "biome check",
		language:   "JavaScript",
		extensions: &["js", "jsx", "ts", "tsx", "json"],
		markers:    &["biome.json", "biome.jsonc", "package.json"],
		binaries:   &["biome"],
		args:       &["check", "--reporter=json", "."],
		parser:     ParserKind::Biome,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "oxlint",
		label:      "oxlint",
		language:   "JavaScript",
		extensions: &["js", "jsx", "ts", "tsx"],
		markers:    &["package.json", ".oxlintrc.json"],
		binaries:   &["oxlint"],
		args:       &["."],
		parser:     ParserKind::Oxlint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "deno-lint",
		label:      "deno lint",
		language:   "JavaScript",
		extensions: &["js", "jsx", "ts", "tsx"],
		markers:    &["deno.json", "deno.jsonc"],
		binaries:   &["deno"],
		args:       &["lint", "--json"],
		parser:     ParserKind::DenoLint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "stylelint",
		label:      "stylelint",
		language:   "CSS",
		extensions: &["css", "scss", "sass", "less"],
		markers:    &["package.json", ".stylelintrc", "stylelint.config.js"],
		binaries:   &["stylelint"],
		args:       &["**/*.{css,scss,sass,less}", "--formatter=json"],
		parser:     ParserKind::Stylelint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "rubocop",
		label:      "rubocop",
		language:   "Ruby",
		extensions: &["rb", "rake"],
		markers:    &["Gemfile", ".rubocop.yml"],
		binaries:   &["rubocop"],
		args:       &["--format", "json"],
		parser:     ParserKind::Rubocop,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "phpstan",
		label:      "phpstan",
		language:   "PHP",
		extensions: &["php"],
		markers:    &["composer.json", "phpstan.neon", "phpstan.neon.dist"],
		binaries:   &["phpstan"],
		args:       &["analyse", "--error-format=json"],
		parser:     ParserKind::Phpstan,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "psalm",
		label:      "psalm",
		language:   "PHP",
		extensions: &["php"],
		markers:    &["composer.json", "psalm.xml", "psalm.xml.dist"],
		binaries:   &["psalm"],
		args:       &["--output-format=json"],
		parser:     ParserKind::Psalm,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "swiftlint",
		label:      "swiftlint",
		language:   "Swift",
		extensions: &["swift"],
		markers:    &["Package.swift", ".swiftlint.yml"],
		binaries:   &["swiftlint"],
		args:       &["lint", "--reporter", "json"],
		parser:     ParserKind::Swiftlint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "dart-analyze",
		label:      "dart analyze",
		language:   "Dart",
		extensions: &["dart"],
		markers:    &["pubspec.yaml"],
		binaries:   &["dart"],
		args:       &["analyze", "--format", "machine"],
		parser:     ParserKind::Dart,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "credo",
		label:      "mix credo",
		language:   "Elixir",
		extensions: &["ex", "exs"],
		markers:    &["mix.exs"],
		binaries:   &["mix"],
		args:       &["credo", "--format", "json"],
		parser:     ParserKind::Credo,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "shellcheck",
		label:      "shellcheck",
		language:   "Shell",
		extensions: &["sh", "bash", "ksh", "zsh"],
		markers:    &[],
		binaries:   &["shellcheck"],
		args:       &["--format=json"],
		parser:     ParserKind::Shellcheck,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "hlint",
		label:      "hlint",
		language:   "Haskell",
		extensions: &["hs", "lhs"],
		markers:    &["stack.yaml", "cabal.project"],
		binaries:   &["hlint"],
		args:       &[".", "--json"],
		parser:     ParserKind::Hlint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "terraform-validate",
		label:      "terraform validate",
		language:   "Terraform",
		extensions: &["tf"],
		markers:    &[".terraform.lock.hcl"],
		binaries:   &["terraform"],
		args:       &["validate", "-json"],
		parser:     ParserKind::Terraform,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "tflint",
		label:      "tflint",
		language:   "Terraform",
		extensions: &["tf"],
		markers:    &[".tflint.hcl"],
		binaries:   &["tflint"],
		args:       &["--format=json"],
		parser:     ParserKind::Tflint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "luacheck",
		label:      "luacheck",
		language:   "Lua",
		extensions: &["lua"],
		markers:    &[".luacheckrc"],
		binaries:   &["luacheck"],
		args:       &["."],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "clang-tidy",
		label:      "clang-tidy",
		language:   "C/C++",
		extensions: &["c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx"],
		markers:    &["compile_commands.json", "CMakeLists.txt"],
		binaries:   &["clang-tidy"],
		args:       &[],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "dotnet-build",
		label:      "dotnet build",
		language:   ".NET",
		extensions: &["cs", "fs", "vb"],
		markers:    &["global.json"],
		binaries:   &["dotnet"],
		args:       &["build", "--no-restore"],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "zig-build",
		label:      "zig build",
		language:   "Zig",
		extensions: &["zig"],
		markers:    &["build.zig"],
		binaries:   &["zig"],
		args:       &["build"],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "gradle-check",
		label:      "Gradle check",
		language:   "JVM",
		extensions: &["java", "kt", "kts", "scala"],
		markers:    &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"],
		binaries:   &["gradle", "gradlew"],
		args:       &["check"],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "maven-verify",
		label:      "Maven verify",
		language:   "JVM",
		extensions: &["java", "kt", "scala"],
		markers:    &["pom.xml"],
		binaries:   &["mvn", "mvnw"],
		args:       &["verify", "-DskipTests"],
		parser:     ParserKind::Generic,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
	Family {
		id:         "actionlint",
		label:      "actionlint",
		language:   "GitHub Actions",
		extensions: &["yml", "yaml"],
		markers:    &[".github/workflows"],
		binaries:   &["actionlint"],
		args:       &["-format", "{{json .}}"],
		parser:     ParserKind::Actionlint,
		effect:     CheckerEffect::ReadOnly,
		test:       false,
	},
];

/// Discovers the full checker matrix from a bounded project file snapshot.
pub fn discover(
	project_root: &Path,
	files: &[PathBuf],
	resolver: &impl BinaryResolver,
	include_tests: bool,
) -> Suite {
	let relative = files
		.iter()
		.filter_map(|path| {
			path
				.strip_prefix(project_root)
				.ok()
				.or(Some(path.as_path()))
		})
		.collect::<Vec<_>>();
	let mut suite = Suite::default();
	let mut ids = BTreeSet::new();
	for family in FAMILIES {
		if family.test && !include_tests {
			continue;
		}
		let extension_present = relative.iter().any(|path| {
			path
				.extension()
				.and_then(|value| value.to_str())
				.is_some_and(|extension| family.extensions.contains(&extension))
		});
		if !extension_present {
			continue;
		}
		let roots = manifest_roots(project_root, &relative, family.markers);
		for root in roots {
			let id = Str::from(if root == project_root {
				family.id.to_owned()
			} else {
				format!(
					"{}-{}",
					family.id,
					root
						.strip_prefix(project_root)
						.unwrap_or(&root)
						.to_string_lossy()
						.replace(['/', '\\'], "-")
				)
			});
			if !ids.insert(id.clone()) {
				continue;
			}
			let Some(binary) = resolver.resolve(family.binaries) else {
				suite.skipped.push(SkippedCheck {
					label:    family.label.into(),
					language: family.language.into(),
					reason:   sf!("required executable unavailable"),
				});
				continue;
			};
			let mut args = family
				.args
				.iter()
				.copied()
				.map(Str::from)
				.collect::<Vec<_>>();
			if family.id == "shellcheck" {
				args.extend(
					relative
						.iter()
						.filter(|path| {
							path
								.extension()
								.and_then(|value| value.to_str())
								.is_some_and(|extension| family.extensions.contains(&extension))
						})
						.map(|path| Str::from(path.to_string_lossy().as_ref())),
				);
			} else if family.id == "clang-tidy" {
				args.extend(
					relative
						.iter()
						.filter(|path| {
							path
								.extension()
								.and_then(|value| value.to_str())
								.is_some_and(|extension| family.extensions.contains(&extension))
						})
						.map(|path| Str::from(path.to_string_lossy().as_ref())),
				);
			}
			suite.checkers.push(Checker {
				id,
				label: family.label.into(),
				language: family.language.into(),
				cwd: root,
				binary,
				args,
				parser: family.parser,
				effect: family.effect,
				test: family.test,
			});
		}
	}
	suite
}

fn manifest_roots(project_root: &Path, files: &[&Path], markers: &[&str]) -> Vec<PathBuf> {
	let mut roots = BTreeSet::new();
	if markers.is_empty() {
		roots.insert(project_root.to_path_buf());
	}
	for file in files {
		let text = file.to_string_lossy().replace('\\', "/");
		let basename = file
			.file_name()
			.and_then(|value| value.to_str())
			.unwrap_or_default();
		if markers
			.iter()
			.any(|marker| basename == *marker || text.contains(marker))
		{
			let root = if file.is_dir() {
				*file
			} else {
				file.parent().unwrap_or(Path::new(""))
			};
			roots.insert(project_root.join(root));
		}
	}
	if roots.is_empty() {
		roots.insert(project_root.to_path_buf());
	}
	roots
		.iter()
		.filter(|root| !roots_have_ancestor(root, project_root, &roots))
		.cloned()
		.collect()
}

fn roots_have_ancestor(root: &Path, project_root: &Path, roots: &BTreeSet<PathBuf>) -> bool {
	root
		.ancestors()
		.skip(1)
		.take_while(|ancestor| *ancestor != project_root)
		.any(|ancestor| roots.contains(ancestor))
}

/// Strict discovery-agent checker specification.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomCheckerSpec {
	/// Stable id.
	pub id:       Str,
	/// Display label.
	pub label:    Str,
	/// Language family.
	pub language: Str,
	/// Manifest-root path relative to the project.
	#[serde(default)]
	pub cwd:      Option<Str>,
	/// Executable name, resolved through project/system authority.
	pub binary:   Str,
	/// Fixed argv.
	#[serde(default)]
	pub args:     Vec<Str>,
	/// Declared parser.
	pub parser:   ParserKind,
	/// Whether execution mutates files.
	#[serde(default)]
	pub mutating: bool,
}

/// Schema-validates freeform checker discovery output.
pub fn parse_custom_specs(json: &str) -> Result<Vec<CustomCheckerSpec>, serde_json::Error> {
	serde_json::from_str(json)
}

/// Resolves schema-valid custom checker proposals without shell interpretation.
pub fn custom_suite(
	project_root: &Path,
	specs: Vec<CustomCheckerSpec>,
	resolver: &impl BinaryResolver,
) -> Suite {
	let mut suite = Suite::default();
	for spec in specs {
		let cwd_escapes = spec.cwd.as_deref().is_some_and(|cwd| {
			Path::new(cwd).is_absolute()
				|| Path::new(cwd)
					.components()
					.any(|component| matches!(component, std::path::Component::ParentDir))
		});
		let cwd = spec
			.cwd
			.as_deref()
			.map_or_else(|| project_root.to_path_buf(), |path| project_root.join(path));
		if cwd_escapes || spec.binary.bytes().any(|byte| matches!(byte, b'/' | b'\\')) {
			suite.skipped.push(SkippedCheck {
				label:    spec.label,
				language: spec.language,
				reason:   sf!("checker path escapes the project or executable authority"),
			});
			continue;
		}
		let Some(binary) = resolver.resolve(&[spec.binary.as_str()]) else {
			suite.skipped.push(SkippedCheck {
				label:    spec.label,
				language: spec.language,
				reason:   sf!("required executable unavailable"),
			});
			continue;
		};
		suite.checkers.push(Checker {
			id: spec.id,
			label: spec.label,
			language: spec.language,
			cwd,
			binary,
			args: spec.args,
			parser: spec.parser,
			effect: if spec.mutating {
				CheckerEffect::Mutating
			} else {
				CheckerEffect::ReadOnly
			},
			test: false,
		});
	}
	suite
}

/// Runs a suite and optionally streams each diagnostic exactly once.
///
/// Mutating checkers run first and withhold diagnostics until every mutator has
/// exited, preventing repair workers from racing formatter writes. Read-only
/// checkers then run concurrently and may emit diagnostics from partial output.
pub async fn run_suite_streaming<R: CheckerRunner>(
	project_root: &Path,
	suite: &Suite,
	runner: &R,
	cancel: &CancellationToken,
	diagnostics: Option<flume::Sender<Vec<Diagnostic>>>,
) -> Result<Report, R::Error> {
	let mut checks = Vec::with_capacity(suite.checkers.len());
	for checker in suite
		.checkers
		.iter()
		.filter(|checker| checker.effect == CheckerEffect::Mutating)
	{
		let process = runner.run_checker(checker, cancel, None).await?;
		checks.push(normalize_result(project_root, checker, process));
	}
	if let Some(sender) = diagnostics.as_ref() {
		for check in &checks {
			let found = check.diagnostics();
			if !found.is_empty() {
				let _ = sender.send(found);
			}
		}
	}
	let read_only = suite
		.checkers
		.iter()
		.filter(|checker| checker.effect == CheckerEffect::ReadOnly);
	let mut read_only_checks = stream::iter(read_only.map(|checker| {
		run_checker_streaming(project_root, checker, runner, cancel, diagnostics.clone())
	}))
	.buffer_unordered(suite.checkers.len().max(1))
	.collect::<Vec<_>>()
	.await
	.into_iter()
	.collect::<Result<Vec<_>, R::Error>>()?;
	checks.append(&mut read_only_checks);
	let order = suite
		.checkers
		.iter()
		.enumerate()
		.map(|(index, checker)| (checker.id.clone(), index))
		.collect::<std::collections::HashMap<_, _>>();
	checks.sort_by_key(|check| order.get(&check.checker.id).copied().unwrap_or(usize::MAX));
	let diagnostics = checks.iter().flat_map(CheckResult::diagnostics).collect();
	Ok(Report { checks, diagnostics, skipped: suite.skipped.clone() })
}

/// Runs a suite without diagnostic streaming.
pub async fn run_suite<R: CheckerRunner>(
	project_root: &Path,
	suite: &Suite,
	runner: &R,
	cancel: &CancellationToken,
) -> Result<Report, R::Error> {
	run_suite_streaming(project_root, suite, runner, cancel, None).await
}

async fn run_checker_streaming<R: CheckerRunner>(
	project_root: &Path,
	checker: &Checker,
	runner: &R,
	cancel: &CancellationToken,
	diagnostics: Option<flume::Sender<Vec<Diagnostic>>>,
) -> Result<CheckResult, R::Error> {
	let (partial_tx, partial_rx) = flume::unbounded();
	let process = runner.run_checker(checker, cancel, diagnostics.as_ref().map(|_| partial_tx));
	tokio::pin!(process);
	let mut emitted = HashSet::new();
	let mut partials_open = diagnostics.is_some();
	let output = loop {
		tokio::select! {
			result = &mut process => break result?,
			partial = partial_rx.recv_async(), if partials_open => {
				let Ok(partial) = partial else {
					partials_open = false;
					continue;
				};
				// Partial output streams only what it already reports: whether the
				// run failed or is unreadable is for the complete output to say.
				if let Ok(found) = parse_output(project_root, checker, &partial) {
					emit_fresh(found, &mut emitted, diagnostics.as_ref());
				}
			},
		}
	};
	let check = normalize_result(project_root, checker, output);
	emit_fresh(check.diagnostics(), &mut emitted, diagnostics.as_ref());
	Ok(check)
}

fn emit_fresh(
	diagnostics: Vec<Diagnostic>,
	emitted: &mut HashSet<Str>,
	sender: Option<&flume::Sender<Vec<Diagnostic>>>,
) {
	let fresh = diagnostics
		.into_iter()
		.filter(|diagnostic| emitted.insert(diagnostic_key(diagnostic)))
		.collect::<Vec<_>>();
	if !fresh.is_empty()
		&& let Some(sender) = sender
	{
		let _ = sender.send(fresh);
	}
}

/// Stable identity used for exactly-once checker delivery and dispatch.
pub fn diagnostic_key(diagnostic: &Diagnostic) -> Str {
	Str::from(format!(
		"{}\0{}\0{}\0{}\0{}",
		diagnostic.file.as_deref().unwrap_or_default(),
		diagnostic
			.line
			.map_or_else(String::new, |value| value.to_string()),
		diagnostic
			.column
			.map_or_else(String::new, |value| value.to_string()),
		diagnostic.code.as_deref().unwrap_or_default(),
		diagnostic.message,
	))
}

fn normalize_result(project_root: &Path, checker: &Checker, process: ProcessOutput) -> CheckResult {
	CheckResult {
		checker:   checker.clone(),
		exit_code: process.exit_code,
		parsed:    parse_output(project_root, checker, &process),
		logs:      process.logs,
	}
}

fn parse_output(
	project_root: &Path,
	checker: &Checker,
	process: &ProcessOutput,
) -> Result<Vec<Diagnostic>, ParseError> {
	parse(checker.parser, &ParserInput {
		checker: checker.id.as_str(),
		checker_root: &checker.cwd,
		project_root,
		stdout: process.stdout.as_str(),
		stderr: process.stderr.as_str(),
	})
}

#[cfg(test)]
mod tests {
	use std::os::unix::fs::PermissionsExt as _;

	use super::*;

	fn executable(path: &Path) {
		fs::create_dir_all(path.parent().expect("parent")).expect("directory");
		fs::write(path, "#!/bin/sh\n").expect("executable");
		fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("mode");
	}

	/// A cloned repository cannot shadow a `$PATH` executable: project-root
	/// and package-manager bins are never candidates, and a relative `$PATH`
	/// entry, which names the command's own directory, is skipped.
	#[test]
	fn only_absolute_path_directories_resolve() {
		let project = tempfile::tempdir().expect("project");
		let tools = tempfile::tempdir().expect("tools");
		for local in ["cargo", "node_modules/.bin/cargo", ".venv/bin/cargo", "vendor/bin/cargo"] {
			executable(&project.path().join(local));
		}
		executable(&tools.path().join("cargo"));
		// The checker runs in the project, where `.` names its root `cargo`.
		env::set_current_dir(project.path()).expect("project cwd");
		let path =
			env::join_paths([Path::new("."), Path::new(""), tools.path()]).expect("PATH value");
		assert_eq!(resolve_on_path(Some(&path), &["cargo"]), Some(tools.path().join("cargo")),);
		let relative_only = env::join_paths([Path::new(".")]).expect("PATH value");
		assert_eq!(resolve_on_path(Some(&relative_only), &["cargo"]), None);
		assert_eq!(resolve_on_path(None, &["cargo"]), None);
	}

	/// A run that established no result, a recorded clean `cargo check` that
	/// exited non-zero, ended by a signal, or stopped before `build-finished`,
	/// projects to one project-wide diagnostic saying why and citing its
	/// complete retained streams. The same run exiting 0 projects to none.
	#[test]
	fn unresolved_runs_project_one_diagnostic_citing_their_logs() {
		let root = Path::new("/private/tmp/cargo-json-repro/clean");
		let stdout = include_str!("../../../../fixtures/cargo/check-clean.stdout");
		let stderr = include_str!("../../../../fixtures/cargo/check-clean.stderr");
		let checker = Checker {
			id:       sf!("cargo-check"),
			label:    sf!("cargo check"),
			language: sf!("Rust"),
			cwd:      root.to_path_buf(),
			binary:   PathBuf::from("cargo"),
			args:     Vec::new(),
			parser:   ParserKind::Rust,
			effect:   CheckerEffect::ReadOnly,
			test:     false,
		};
		let rows = |exit_code, stdout: &str| {
			let logs = CheckLogs::of(stdout.as_bytes(), stderr.as_bytes());
			let check = normalize_result(root, &checker, ProcessOutput {
				exit_code,
				stdout: Str::new(stdout),
				stderr: Str::new(stderr),
				logs: Some(logs),
			});
			let cited = format!(
				"\n[stdout: artifact://sha256/{}; {} bytes]\n[stderr: artifact://sha256/{}; {} bytes]",
				logs.stdout,
				stdout.len(),
				logs.stderr,
				stderr.len()
			);
			let rows = check
				.diagnostics()
				.into_iter()
				.map(|diagnostic| (diagnostic.file, diagnostic.code, diagnostic.message.to_string()))
				.collect::<Vec<_>>();
			(rows, cited)
		};
		assert!(rows(Some(0), stdout).0.is_empty());
		let (failed, cited) = rows(Some(101), stdout);
		assert_eq!(failed, [(
			None,
			Some(sf!("checker-failed")),
			format!("Checker exited with status 101 without reporting a diagnostic{cited}")
		)]);
		let (killed, cited) = rows(None, stdout);
		assert_eq!(killed, [(
			None,
			Some(sf!("checker-failed")),
			format!("Checker ended without an exit status and reported no diagnostic{cited}")
		)]);
		let finished = stdout
			.find("{\"reason\":\"build-finished\"")
			.expect("build-finished record");
		let (unfinished, cited) = rows(Some(0), &stdout[..finished]);
		assert_eq!(unfinished, [(
			None,
			Some(sf!("unparseable")),
			format!("cargo wrote no `build-finished` record and no diagnostic{cited}")
		)]);
	}
}
