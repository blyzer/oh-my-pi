//! The asset steps: v1 user files copied into the v2 profile configuration
//! root where v2 discovery reads them.
//!
//! | v1 (agent dir)                          | v2 (profile config root)   | v2 reader                                        |
//! |-----------------------------------------|----------------------------|--------------------------------------------------|
//! | `skills/`, `managed-skills/`            | `agent/skills/`, `agent/managed-skills/` | `discovery::skills::{sources, managed_skills_root}` |
//! | `rules/`, `RULES.md`, `AGENTS.md`       | `agent/rules/`, `agent/RULES.md`, `agent/AGENTS.md` | `discovery::rules`              |
//! | `prompts/`, `commands/*.md`             | `agent/prompts/`           | `discovery::prompts::PromptTemplates::discover`  |
//! | `themes/`                               | `agent/themes/`            | `omp-app` `resolve_theme`                        |
//! | `SYSTEM.md`, `APPEND_SYSTEM.md`, `TITLE_SYSTEM.md` | `agent/<name>`  | `prompt_input::user_prompt_path`                 |
//! | `lsp.*`, `dap.*` (six spellings each)   | `agent/<same name>`        | `omp_envd` `discover_native_{lsp,dap}_sources`   |
//! | `secrets.yml`                           | `secrets.yml` (not under `agent/`) | `omp-app` `secrets_files`                |
//! | `mcp.json`, `.mcp.json`                 | `mcp.json` (merged)        | `omp_envd::mcp::McpConfigPaths::user`            |
//! | `ssh.json`                              | `hosts.toml` (converted)   | `omp_envd::ssh::HostPaths::user`                 |
//!
//! The working directory's project `.omp/` is read by v2 at the same paths,
//! except three v1-only files [`import_project_assets`] converts in place.
//!
//! Copy rules (owner decision #2): nothing under v1 is written. A destination
//! v2 already has is kept: identical bytes (or the same declaration) are
//! [`SkipReason::AlreadyPresent`], different ones [`Attention::Conflict`]. A
//! tree copies file by file under the same rule. A file v2's own parser
//! rejects is [`Attention::Incompatible`] and is not copied.

mod mcp;
mod project;
mod ssh;

pub use project::{import_project_assets, project_assets_marker};

#[cfg(test)]
mod tests;

use std::{
	fs, io,
	path::{Path, PathBuf},
};

use omp_core::{FastHashSet, Str};
use omp_envd::{
	docserver::{
		dap_adapter::builtin_adapters,
		dap_config::{DapConfigError, DapConfigSource, DapConfigSourceKind, load_dap_config},
		lsp_config::{
			LspConfigError, LspConfigSource, LspConfigSourceKind, bundled_lsp_defaults,
			load_lsp_config,
		},
	},
	mcp::{config::ConfigValidationError, config_store::ConfigStoreError},
	ssh::SshError,
};
use thiserror::Error;

use super::{
	ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, StepContext, V1Item,
	report::{Attention, NotMigratable, SkipReason},
};
use crate::{
	discovery::{prompts::check_template, skills::managed_skills_root},
	secrets::config::{SecretConfigError, load_secret_rules},
};

/// Why an asset could not be read, checked, or written.
///
/// Read and write failures fail the step (it retries next run); the others
/// are reported as [`Attention::Incompatible`] for the one file or entry.
#[derive(Debug, Error)]
pub enum AssetError {
	/// A v1 file or a v2 destination could not be read.
	#[error("could not read {}", path.display())]
	Read {
		/// The path read.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A v2 destination could not be written.
	#[error("could not write {}", path.display())]
	Write {
		/// The path written.
		path:   PathBuf,
		/// Filesystem failure.
		#[source]
		source: io::Error,
	},
	/// A prompt template is not UTF-8, which v2 discovery requires.
	#[error("the prompt template is not UTF-8")]
	NotUtf8(#[source] std::str::Utf8Error),
	/// v2 discovery would reject a prompt template's frontmatter.
	#[error("v2 cannot parse the prompt template frontmatter")]
	PromptFrontmatter(#[source] serde_yaml::Error),
	/// The LSP file does not load under v2's schema.
	#[error("the LSP configuration does not load in v2")]
	Lsp(#[source] LspConfigError),
	/// The DAP file does not load under v2's schema.
	#[error("the DAP configuration does not load in v2")]
	Dap(#[source] DapConfigError),
	/// The secret rules do not load in v2.
	#[error("the secret rules do not load in v2")]
	Secrets(#[source] SecretConfigError),
	/// The v1 MCP file is not a valid MCP document.
	#[error("the MCP configuration is not a valid MCP document")]
	McpDocument(#[source] serde_json::Error),
	/// One v1 MCP server fails v2's validation.
	#[error("the MCP server fails v2 validation")]
	McpServer(#[source] ConfigValidationError),
	/// The v2 MCP file could not be read or written.
	#[error("could not update the v2 MCP configuration")]
	McpStore(#[source] ConfigStoreError),
	/// The v1 SSH file is not a valid host list.
	#[error("the SSH configuration is not a valid host list")]
	SshDocument(#[source] serde_json::Error),
	/// A v1 host has no address.
	#[error("the SSH host has no `host`")]
	SshNoAddress,
	/// A v1 host has no user name; v2 needs one.
	#[error("the SSH host has no `username`; v2 needs one")]
	SshNoUser,
	/// A v1 port is not a TCP port number.
	#[error("the SSH host's `port` is not a port number")]
	SshPort,
	/// A v1 value relied on `${VAR}` expansion, which v2 does not do.
	#[error("the SSH host uses `${{VAR}}` expansion, which v2 does not do")]
	SshEnvironment,
	/// `~/.ssh/known_hosts` could not be read.
	#[error("could not look up the host key")]
	SshKnownHosts(#[source] SshError),
	/// v2 pins every host key; v1 relied on `ssh`'s own checking.
	#[error(
		"no ~/.ssh/known_hosts entry pins the host key; connect once with `ssh` or set `host_key` \
		 in hosts.toml"
	)]
	SshNoHostKey,
	/// The converted host fails v2's validation.
	#[error("the converted SSH host fails v2 validation")]
	SshHost(#[source] SshError),
	/// The v2 hosts file could not be read or written.
	#[error("could not update the v2 SSH hosts")]
	SshStore(#[source] SshError),
}

const _: () = assert!(size_of::<AssetError>() <= 112, "AssetError must stay compact");

/// The items a grouped step imports besides its own [`ImportStep::item`].
pub(super) const fn companions(step: ImportStep) -> &'static [V1Item] {
	match step {
		ImportStep::Skills => &[V1Item::ManagedSkills],
		ImportStep::Rules => &[V1Item::RulesMd, V1Item::AgentsMd],
		ImportStep::SystemPrompts => &[V1Item::AppendSystemMd, V1Item::TitleSystemMd],
		ImportStep::LspDap => &[V1Item::Dap],
		_ => &[],
	}
}

/// Runs one asset step for one profile pair.
pub(super) fn import(
	step: ImportStep,
	cx: &StepContext<'_>,
) -> Result<Vec<ImportEntry>, ImportError> {
	let mut out = Entries { step, mode: cx.mode, list: Vec::new() };
	let source = &cx.pair.source;
	let config = cx.pair.target.config_dir.as_path();
	let agent = config.join("agent");
	match step {
		ImportStep::Skills => {
			out.tree(V1Item::Skills, source.locate(V1Item::Skills), config, &agent.join("skills"))?;
			out.tree(
				V1Item::ManagedSkills,
				source.locate(V1Item::ManagedSkills),
				config,
				&managed_skills_root(config),
			)?;
		},
		ImportStep::Rules => {
			out.tree(V1Item::Rules, source.locate(V1Item::Rules), config, &agent.join("rules"))?;
			for (item, name) in [(V1Item::RulesMd, "RULES.md"), (V1Item::AgentsMd, "AGENTS.md")] {
				out.file(item, source.locate(item), config, &agent.join(name), Check::Verbatim)?;
			}
		},
		ImportStep::Prompts => {
			out.tree(V1Item::Prompts, source.locate(V1Item::Prompts), config, &agent.join("prompts"))?;
		},
		ImportStep::Commands => {
			out.commands(
				V1Item::Commands,
				source.locate(V1Item::Commands),
				config,
				&agent.join("prompts"),
			)?;
		},
		ImportStep::Themes => {
			out.tree(V1Item::Themes, source.locate(V1Item::Themes), config, &agent.join("themes"))?;
		},
		ImportStep::SystemPrompts => {
			for (item, name) in [
				(V1Item::SystemMd, "SYSTEM.md"),
				(V1Item::AppendSystemMd, "APPEND_SYSTEM.md"),
				(V1Item::TitleSystemMd, "TITLE_SYSTEM.md"),
			] {
				out.file(item, source.locate(item), config, &agent.join(name), Check::Verbatim)?;
			}
		},
		ImportStep::LspDap => {
			for (item, check) in [(V1Item::Lsp, Check::Lsp), (V1Item::Dap, Check::Dap)] {
				// v2 merges every spelling under `agent/`, as v1 did.
				let found = source
					.candidates(item)
					.filter(|path| path.is_file())
					.collect::<Vec<_>>();
				if found.is_empty() {
					out.push(item, None, None, ImportOutcome::NothingToImport);
				}
				for path in found {
					let destination = agent.join(path.file_name().unwrap_or_default());
					out.file(item, Some(path), config, &destination, check)?;
				}
			}
		},
		ImportStep::Secrets => {
			out.file(
				V1Item::Secrets,
				source.locate(V1Item::Secrets),
				config,
				&config.join("secrets.yml"),
				Check::Secrets,
			)?;
		},
		ImportStep::Mcp => {
			let found = source
				.candidates(V1Item::Mcp)
				.filter(|path| path.is_file())
				.collect::<Vec<_>>();
			mcp::merge(&mut out, V1Item::Mcp, &found, config, &config.join("mcp.json"))?;
		},
		ImportStep::SshHosts => {
			ssh::convert(
				&mut out,
				V1Item::Ssh,
				source.locate(V1Item::Ssh),
				config,
				&config.join("hosts.toml"),
				source.home(),
			)?;
		},
		// Not an asset step; `ImportStep::run` never routes one here.
		_ => return Ok(Vec::new()),
	}
	if cx.mode == ImportMode::Apply {
		let marker = step.marker(config);
		marker
			.set(None)
			.map_err(|source| AssetError::Write { path: marker.path().to_owned(), source })?;
	}
	Ok(out.list)
}

/// A pre-copy check with v2's own parser.
#[derive(Clone, Copy, Debug)]
enum Check {
	/// Copied as is.
	Verbatim,
	/// Markdown prompt templates: the frontmatter discovery parses.
	PromptTemplate,
	/// An LSP file over the bundled catalog, as the supervisor loads it.
	Lsp,
	/// A DAP file over the built-in adapters, as the daemon loads it.
	Dap,
	/// A `secrets.yml` rule list.
	Secrets,
}

impl Check {
	/// `Err` names why v2 could not read `path`.
	fn run(self, path: &Path) -> Result<(), AssetError> {
		match self {
			Self::Verbatim => Ok(()),
			Self::PromptTemplate => {
				if !is_markdown(path) {
					return Ok(());
				}
				let bytes = fs::read(path)
					.map_err(|source| AssetError::Read { path: path.to_owned(), source })?;
				let text = std::str::from_utf8(&bytes).map_err(AssetError::NotUtf8)?;
				check_template(text).map_err(AssetError::PromptFrontmatter)
			},
			Self::Lsp => {
				let source =
					LspConfigSource::read(LspConfigSourceKind::User, path).map_err(AssetError::Lsp)?;
				let bundled = bundled_lsp_defaults().map_err(AssetError::Lsp)?;
				load_lsp_config(&[bundled, source])
					.map(drop)
					.map_err(AssetError::Lsp)
			},
			Self::Dap => {
				let source =
					DapConfigSource::read(DapConfigSourceKind::User, path).map_err(AssetError::Dap)?;
				let adapters =
					load_dap_config(builtin_adapters(), &[source]).map_err(AssetError::Dap)?;
				adapters
					.values()
					.try_for_each(|adapter| adapter.to_spec().map(drop))
					.map_err(AssetError::Dap)
			},
			Self::Secrets => load_secret_rules(path, path)
				.map(drop)
				.map_err(AssetError::Secrets),
		}
	}
}

fn is_markdown(path: &Path) -> bool {
	path
		.extension()
		.is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// What placing one file did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Placed {
	/// The destination was free: copied (or, dry, would be).
	Copied,
	/// The destination already holds the same bytes.
	Present,
	/// The destination holds something else, which is kept.
	Conflict,
}

/// Copies `source` to a free `destination`; never replaces anything.
fn place(source: &Path, destination: &Path, mode: ImportMode) -> Result<Placed, AssetError> {
	match fs::metadata(destination) {
		Ok(metadata) if metadata.is_file() => {
			let read = |path: &Path| {
				fs::read(path)
					.map_err(|error| AssetError::Read { path: path.to_owned(), source: error })
			};
			Ok(if read(source)? == read(destination)? {
				Placed::Present
			} else {
				Placed::Conflict
			})
		},
		Ok(_) => Ok(Placed::Conflict),
		Err(error) if error.kind() == io::ErrorKind::NotADirectory => Ok(Placed::Conflict),
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			if mode == ImportMode::Apply {
				copy_new(source, destination)?;
			}
			Ok(Placed::Copied)
		},
		Err(error) => Err(AssetError::Read { path: destination.to_owned(), source: error }),
	}
}

/// Maps a write failure at `path`.
fn write(path: &Path) -> impl FnOnce(io::Error) -> AssetError + '_ {
	move |source| AssetError::Write { path: path.to_owned(), source }
}

/// Copies through a sibling temporary file, keeping the permission bits.
fn copy_new(source: &Path, destination: &Path) -> Result<(), AssetError> {
	if let Some(parent) = destination.parent() {
		fs::create_dir_all(parent).map_err(write(parent))?;
	}
	let mut temporary = destination.as_os_str().to_owned();
	temporary.push(format!(".v1-import-{}", std::process::id()));
	let temporary = PathBuf::from(temporary);
	let result = fs::copy(source, &temporary)
		.map_err(write(&temporary))
		.and_then(|_| fs::rename(&temporary, destination).map_err(write(destination)));
	if result.is_err() {
		let _ = fs::remove_file(&temporary);
	}
	result
}

/// The entries of `directory`, sorted, hidden names included.
fn children(directory: &Path) -> Result<Vec<PathBuf>, AssetError> {
	let read = |error| AssetError::Read { path: directory.to_owned(), source: error };
	let mut paths = fs::read_dir(directory)
		.map_err(read)?
		.map(|entry| entry.map(|entry| entry.path()))
		.collect::<Result<Vec<_>, _>>()
		.map_err(read)?;
	paths.sort();
	Ok(paths)
}

/// Every file under `root` with its path relative to `root`, sorted.
/// Symbolic links are followed; a directory reached twice is walked once.
fn walk(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, AssetError> {
	let canonical = |path: &Path| {
		fs::canonicalize(path).map_err(|source| AssetError::Read { path: path.to_owned(), source })
	};
	let mut seen = FastHashSet::default();
	seen.insert(canonical(root)?);
	let mut pending = vec![(root.to_owned(), PathBuf::new())];
	let mut files = Vec::new();
	while let Some((directory, relative)) = pending.pop() {
		for path in children(&directory)? {
			let Some(name) = path.file_name() else {
				continue;
			};
			let relative = relative.join(name);
			let metadata = fs::metadata(&path)
				.map_err(|source| AssetError::Read { path: path.clone(), source })?;
			if metadata.is_dir() {
				if seen.insert(canonical(&path)?) {
					pending.push((path, relative));
				}
			} else {
				files.push((path, relative));
			}
		}
	}
	files.sort_by(|left, right| left.1.cmp(&right.1));
	Ok(files)
}

/// `path` relative to `root`, for report subjects.
fn relative(root: &Path, path: &Path) -> Str {
	Str::new(path.strip_prefix(root).unwrap_or(path).to_string_lossy())
}

/// One step's report entries.
struct Entries {
	step: ImportStep,
	mode: ImportMode,
	list: Vec<ImportEntry>,
}

impl Entries {
	fn push(
		&mut self,
		item: V1Item,
		path: Option<PathBuf>,
		subject: Option<Str>,
		outcome: ImportOutcome,
	) {
		self
			.list
			.push(ImportEntry { step: self.step, item, path, subject, outcome });
	}

	/// The outcome of something copied (or, dry, copyable).
	const fn copied(&self) -> ImportOutcome {
		match self.mode {
			ImportMode::Apply => ImportOutcome::Imported,
			ImportMode::DryRun => ImportOutcome::WouldImport,
		}
	}

	/// The outcome of one placed file.
	const fn placed(&self, placed: Placed) -> ImportOutcome {
		match placed {
			Placed::Copied => self.copied(),
			Placed::Present => ImportOutcome::Skipped(SkipReason::AlreadyPresent),
			Placed::Conflict => ImportOutcome::NeedsAttention(Attention::Conflict),
		}
	}

	/// Copies one v1 file after `check`.
	fn file(
		&mut self,
		item: V1Item,
		source: Option<PathBuf>,
		root: &Path,
		destination: &Path,
		check: Check,
	) -> Result<(), AssetError> {
		let Some(source) = source else {
			self.push(item, None, None, ImportOutcome::NothingToImport);
			return Ok(());
		};
		let subject = Some(relative(root, destination));
		let outcome = match check.run(&source) {
			Err(error) => ImportOutcome::NeedsAttention(Attention::Incompatible(error.into())),
			Ok(()) => self.placed(place(&source, destination, self.mode)?),
		};
		self.push(item, Some(source), subject, outcome);
		Ok(())
	}

	/// Copies a v1 tree file by file: one summary entry for what was copied,
	/// one for what v2 already had, and one per conflict.
	fn tree(
		&mut self,
		item: V1Item,
		source: Option<PathBuf>,
		root: &Path,
		destination: &Path,
	) -> Result<(), AssetError> {
		let Some(source) = source else {
			self.push(item, None, None, ImportOutcome::NothingToImport);
			return Ok(());
		};
		let check = if item == V1Item::Prompts {
			Check::PromptTemplate
		} else {
			Check::Verbatim
		};
		let files = walk(&source)?;
		let (mut copied, mut present) = (0_usize, 0_usize);
		let mut singles = Vec::new();
		for (file, path) in &files {
			let target = destination.join(path);
			let outcome = match check.run(file) {
				Err(error) => ImportOutcome::NeedsAttention(Attention::Incompatible(error.into())),
				Ok(()) => match place(file, &target, self.mode)? {
					Placed::Copied => {
						copied += 1;
						continue;
					},
					Placed::Present => {
						present += 1;
						continue;
					},
					Placed::Conflict => ImportOutcome::NeedsAttention(Attention::Conflict),
				},
			};
			singles.push((file.clone(), relative(root, &target), outcome));
		}
		let counted = |count: usize| {
			let noun = if count == 1 { "file" } else { "files" };
			Some(Str::from(format_args!("{} ({count} {noun})", relative(root, destination))))
		};
		if copied > 0 {
			let outcome = self.copied();
			self.push(item, Some(source.clone()), counted(copied), outcome);
		}
		if present > 0 {
			self.push(
				item,
				Some(source.clone()),
				counted(present),
				ImportOutcome::Skipped(SkipReason::AlreadyPresent),
			);
		}
		if files.is_empty() {
			self.push(item, Some(source), None, ImportOutcome::NothingToImport);
		}
		for (file, subject, outcome) in singles {
			self.push(item, Some(file), Some(subject), outcome);
		}
		Ok(())
	}

	/// v1 slash commands into a prompt-template directory (owner decision
	/// #12). v1 loaded `*.md` directly under `commands/` with the same
	/// `description` frontmatter and argument placeholders v2 templates use,
	/// so each copies as is. `commands/<name>/index.{ts,js,…}` modules have no
	/// v2 runtime.
	fn commands(
		&mut self,
		item: V1Item,
		source: Option<PathBuf>,
		root: &Path,
		destination: &Path,
	) -> Result<(), AssetError> {
		const MODULES: [&str; 4] = ["index.ts", "index.js", "index.mjs", "index.cjs"];
		let Some(source) = source else {
			self.push(item, None, None, ImportOutcome::NothingToImport);
			return Ok(());
		};
		let mut found = false;
		for path in children(&source)? {
			let Some(name) = path.file_name() else {
				continue;
			};
			if name.as_encoded_bytes().starts_with(b".") {
				continue;
			}
			let metadata = fs::metadata(&path)
				.map_err(|error| AssetError::Read { path: path.clone(), source: error })?;
			if metadata.is_file() && is_markdown(&path) {
				found = true;
				let target = destination.join(name);
				let outcome = match Check::PromptTemplate.run(&path) {
					Err(error) => ImportOutcome::NeedsAttention(Attention::Incompatible(error.into())),
					Ok(()) => self.placed(place(&path, &target, self.mode)?),
				};
				self.push(item, Some(path), Some(relative(root, &target)), outcome);
			} else if metadata.is_dir() && MODULES.iter().any(|module| path.join(module).is_file()) {
				found = true;
				let subject = Some(Str::new(name.to_string_lossy()));
				self.push(
					item,
					Some(path),
					subject,
					ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
				);
			}
		}
		if !found {
			self.push(item, Some(source), None, ImportOutcome::NothingToImport);
		}
		Ok(())
	}
}
