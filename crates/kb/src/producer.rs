use std::{
	collections::BTreeMap,
	io,
	path::Path,
	process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use omp_ast::SupportLang;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Run {
	Indexed,
	Absent,
}

pub(crate) struct Producer {
	pub(crate) name: &'static str,
	languages:       &'static [SupportLang],
	program_env:     &'static str,
	program:         &'static str,
	manifests:       &'static [&'static str],
	arguments:       fn(&mut Command, &Path, &Path),
}

fn rust_arguments(command: &mut Command, root: &Path, output: &Path) {
	command
		.arg("scip")
		.arg(root)
		.args(["--exclude-vendored-libraries", "--num-threads", "2", "--output"])
		.arg(output);
}

fn typescript_arguments(command: &mut Command, _root: &Path, output: &Path) {
	command
		.args(["index", "--infer-tsconfig", "--no-progress-bar", "--output"])
		.arg(output);
}

fn python_arguments(command: &mut Command, _root: &Path, output: &Path) {
	command
		.args([
			"index",
			"--project-name",
			"repository",
			"--project-version",
			"0",
			"--quiet",
			"--output",
		])
		.arg(output);
}

pub(crate) static PRODUCERS: [Producer; 3] = [
	Producer {
		name:        "rust-analyzer",
		languages:   &[SupportLang::Rust],
		program_env: "RUST_ANALYZER",
		program:     "rust-analyzer",
		manifests:   &["Cargo.toml", "rust-project.json"],
		arguments:   rust_arguments,
	},
	Producer {
		name:        "scip-typescript",
		languages:   &[SupportLang::TypeScript, SupportLang::Tsx, SupportLang::JavaScript],
		program_env: "OMP_KB_SCIP_TYPESCRIPT",
		program:     "scip-typescript",
		manifests:   &[],
		arguments:   typescript_arguments,
	},
	Producer {
		name:        "scip-python",
		languages:   &[SupportLang::Python],
		program_env: "OMP_KB_SCIP_PYTHON",
		program:     "scip-python",
		manifests:   &[],
		arguments:   python_arguments,
	},
];

impl Producer {
	pub(crate) fn handles(&self, language: SupportLang) -> bool {
		self.languages.contains(&language)
	}

	pub(crate) fn applies(&self, files: &BTreeMap<String, Option<String>>) -> bool {
		self.manifests.is_empty()
			|| self
				.manifests
				.iter()
				.any(|manifest| files.get(*manifest).is_some_and(Option::is_some))
	}

	pub(crate) fn run(&self, root: &Path, output: &Path) -> Result<Run> {
		let program = std::env::var_os(self.program_env).unwrap_or_else(|| self.program.into());
		let mut command = Command::new(program);
		command
			.current_dir(root)
			.stdin(Stdio::null())
			.stdout(Stdio::null());
		(self.arguments)(&mut command, root, output);
		match command.status() {
			Ok(status) if status.success() => Ok(Run::Indexed),
			Ok(status) => bail!("{} failed with {status}", self.name),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Run::Absent),
			Err(error) => Err(error).with_context(|| format!("starting {}", self.name)),
		}
	}
}
