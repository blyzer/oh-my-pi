//! The working directory's project `.omp/`, converted in place.
//!
//! v2 reads a project's `.omp/` at the paths v1 used (`AGENTS.md`,
//! `RULES.md`, `rules/`, `skills/`, `prompts/`, `SYSTEM.md`, `mcp.json`,
//! `secrets.yml`, `lsp.*`, `dap.*`), so those stay as they are. Three v1-only
//! files gain a v2 sibling instead: `ssh.json` → `hosts.toml`, `.mcp.json`
//! merged into `mcp.json`, and `commands/*.md` → `prompts/` (owner decision
//! #12). The v1 files stay untouched.
//!
//! A project is converted once per v2 profile: its marker is
//! `<profile config root>/.project-assets-migration-v1.d/<SHA-256 of the
//! project path>`, so every project is converted the first time v2 runs (or
//! `omp config import-v1` runs) there. A project without `.omp/` has nothing
//! to convert and is not marked.

use std::{
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use omp_core::Hash32;

use super::{AssetError, Entries, mcp, ssh, write};
use crate::v1_import::{
	ImportMode, ImportOutcome, ImportStep, StepContext, V1Item, report::SkipReason,
	step::atomic_replace,
};

/// Converts the pair's project, if it has one.
pub(super) fn import(out: &mut Entries, cx: &StepContext<'_>) -> Result<(), AssetError> {
	let layout = &cx.pair.source;
	let Some(project) = layout.project() else {
		out.push(V1Item::ProjectSsh, None, None, ImportOutcome::NothingToImport);
		return Ok(());
	};
	let omp = project.join(".omp");
	if !omp.is_dir() {
		out.push(V1Item::ProjectSsh, Some(omp), None, ImportOutcome::NothingToImport);
		return Ok(());
	}
	let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
	let project_omp = canonical(&omp);
	if [layout.base_root(), layout.config_root(), layout.agent_dir()]
		.into_iter()
		.any(|v1| project_omp.starts_with(canonical(v1)))
	{
		out.push(
			V1Item::ProjectSsh,
			Some(omp),
			None,
			ImportOutcome::Skipped(SkipReason::InsideV1Root),
		);
		return Ok(());
	}
	let project = canonical(project);
	let marker = marker(&cx.pair.target.config_dir, &project);
	if marker.exists() {
		out.push(
			V1Item::ProjectSsh,
			Some(omp),
			None,
			ImportOutcome::Skipped(SkipReason::MarkerPresent),
		);
		return Ok(());
	}
	// The v2 readers: `HostPaths::project`, `McpConfigPaths::project`, and
	// `PromptTemplates::discover`'s `<project>/.omp/prompts`.
	ssh::convert(
		out,
		V1Item::ProjectSsh,
		layout.locate(V1Item::ProjectSsh),
		&project,
		&omp.join("hosts.toml"),
		layout.home(),
	)?;
	let hidden = layout.locate(V1Item::ProjectMcp);
	mcp::merge(out, V1Item::ProjectMcp, hidden.as_slice(), &project, &omp.join("mcp.json"))?;
	out.commands(
		V1Item::ProjectCommands,
		layout.locate(V1Item::ProjectCommands),
		&project,
		&omp.join("prompts"),
	)?;
	if cx.mode == ImportMode::Apply {
		if let Some(parent) = marker.parent() {
			fs::create_dir_all(parent).map_err(write(parent))?;
		}
		let mut contents = String::with_capacity(64);
		let _ = writeln!(contents, "revision = {}", crate::v1_import::Marker::REVISION);
		let _ = writeln!(contents, "project = {:?}", project.display().to_string());
		atomic_replace(&marker, contents.as_bytes()).map_err(write(&marker))?;
	}
	Ok(())
}

/// This profile's marker for `project`.
fn marker(config_dir: &Path, project: &Path) -> PathBuf {
	let mut directory = ImportStep::ProjectAssets
		.marker(config_dir)
		.path()
		.as_os_str()
		.to_owned();
	directory.push(".d");
	let digest = Hash32::sum(project.as_os_str().as_encoded_bytes());
	PathBuf::from(directory).join(digest.to_hex().as_str())
}
