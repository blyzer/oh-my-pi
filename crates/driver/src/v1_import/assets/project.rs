//! The working directory's project `.omp/`, converted in place.
//!
//! v2 reads a project's `.omp/` at the paths v1 used (`AGENTS.md`,
//! `RULES.md`, `rules/`, `skills/`, `prompts/`, `SYSTEM.md`, `mcp.json`,
//! `secrets.yml`, `lsp.*`, `dap.*`), so those stay as they are. Three v1-only
//! files gain a v2 sibling instead: `ssh.json` → `hosts.toml`, `.mcp.json`
//! merged into `mcp.json`, and `commands/*.md` → `prompts/` (owner decision
//! #12). The v1 files stay untouched.
//!
//! Like the project settings import, this follows the project omp runs in:
//! the first-run hook and `omp config import-v1` convert the current project,
//! once, recorded by a marker under the v2 state root named by the SHA-256 of
//! the project's canonical path ([`project_assets_marker`]). The marker lives
//! outside the repository and is written only for a project that had a v1
//! file.

use std::{
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use omp_core::Hash32;

use super::{AssetError, Entries, mcp, ssh, write};
use crate::v1_import::{
	Attention, ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, SkipReason, V1Item,
	V1Source, V2Roots, step::atomic_replace,
};

/// Converts `project`'s v1-only `.omp/` files, once per project. Never
/// fails: a failure is an [`Attention::Failed`] entry and the next run
/// retries.
#[must_use]
pub fn import_project_assets(
	project: &Path,
	source: &V1Source,
	roots: &V2Roots,
	mode: ImportMode,
) -> Vec<ImportEntry> {
	let omp = project.join(".omp");
	let single = |outcome| {
		vec![ImportEntry::new(ImportStep::SshHosts, V1Item::Ssh, Some(omp.clone()), outcome)]
	};
	if !omp.join("ssh.json").is_file()
		&& !omp.join(".mcp.json").is_file()
		&& !omp.join("commands").is_dir()
	{
		return single(ImportOutcome::NothingToImport);
	}
	// Run from `$HOME` (or inside a v1 root), the project's `.omp/` is the v1
	// install itself, which is never written.
	let layout = source.layout(None);
	let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
	let project_omp = canonical(&omp);
	if [source.base_root(), layout.agent_dir()]
		.into_iter()
		.any(|v1| project_omp.starts_with(canonical(v1)))
	{
		return single(ImportOutcome::Skipped(SkipReason::InsideV1Root));
	}
	let marker = project_assets_marker(project, roots);
	if marker.exists() {
		return single(ImportOutcome::Skipped(SkipReason::MarkerPresent));
	}
	let mut entries = Vec::new();
	let converted = convert(&mut entries, project, &omp, layout.home(), mode).and_then(|()| {
		if mode == ImportMode::Apply {
			write_marker(&marker, project)?;
		}
		Ok(())
	});
	if let Err(error) = converted {
		entries.push(ImportEntry::new(
			ImportStep::SshHosts,
			V1Item::Ssh,
			Some(omp),
			ImportOutcome::NeedsAttention(Attention::Failed(ImportError::Assets(error))),
		));
	}
	entries
}

/// The three conversions, each reported under the user step it mirrors. The
/// v2 readers are `HostPaths::project`, `McpConfigPaths::project`, and
/// `PromptTemplates::discover`'s `<project>/.omp/prompts`.
fn convert(
	entries: &mut Vec<ImportEntry>,
	project: &Path,
	omp: &Path,
	home: &Path,
	mode: ImportMode,
) -> Result<(), AssetError> {
	let file = |name: &str| Some(omp.join(name)).filter(|path| path.is_file());
	let mut out = Entries { step: ImportStep::SshHosts, mode, list: Vec::new() };
	let result =
		ssh::convert(&mut out, V1Item::Ssh, file("ssh.json"), project, &omp.join("hosts.toml"), home);
	entries.append(&mut out.list);
	result?;
	let mut out = Entries { step: ImportStep::Mcp, mode, list: Vec::new() };
	let hidden = file(".mcp.json");
	let result =
		mcp::merge(&mut out, V1Item::Mcp, hidden.as_slice(), project, &omp.join("mcp.json"));
	entries.append(&mut out.list);
	result?;
	let mut out = Entries { step: ImportStep::Commands, mode, list: Vec::new() };
	let commands = Some(omp.join("commands")).filter(|path| path.is_dir());
	let result = out.commands(V1Item::Commands, commands, project, &omp.join("prompts"));
	entries.append(&mut out.list);
	result
}

/// Where the project's asset conversion is recorded: the v2 state root, keyed
/// by the SHA-256 of the project's canonical path.
#[must_use]
pub fn project_assets_marker(project: &Path, roots: &V2Roots) -> PathBuf {
	let canonical = fs::canonicalize(project).unwrap_or_else(|_| project.to_owned());
	let digest = Hash32::sum(canonical.as_os_str().as_encoded_bytes());
	let mut name = String::with_capacity(".project-assets-migration-v1-".len() + 64);
	let _ = write!(name, ".project-assets-migration-v1-{}", digest.to_hex().as_str());
	roots.state_dir.join("v1-import").join(name)
}

fn write_marker(marker: &Path, project: &Path) -> Result<(), AssetError> {
	if let Some(parent) = marker.parent() {
		fs::create_dir_all(parent).map_err(write(parent))?;
	}
	let mut contents = String::with_capacity(64);
	let _ = writeln!(contents, "revision = {}", crate::v1_import::Marker::REVISION);
	let _ = writeln!(contents, "project = {:?}", project.display().to_string());
	atomic_replace(marker, contents.as_bytes()).map_err(write(marker))
}
