//! The working directory's project `.omp/`, converted in place.
//!
//! v2 reads a project's `.omp/` at the paths v1 used (`AGENTS.md`,
//! `RULES.md`, `rules/`, `skills/`, `prompts/`, `SYSTEM.md`, `mcp.json`,
//! `secrets.yml`, `lsp.*`, `dap.*`), so those stay as they are. Three v1-only
//! files gain a v2 sibling instead: `ssh.json` → `hosts.toml`, `.mcp.json`
//! merged into `mcp.json`, and `commands/*.md` → `prompts/` (owner decision
//! #12). v1 also read hosts from `ssh.json` and `.ssh.json` at the project
//! root; they convert into the same `.omp/hosts.toml`, in v1's precedence
//! order (`discovery/ssh.ts`: `.omp/ssh.json`, the user `ssh.json`, then the
//! two root files, the first host of a name winning). The v1 files stay
//! untouched.
//!
//! Like the project settings import, this follows the project omp runs in:
//! the first-run hook and `omp config import-v1` convert the current project,
//! once, recorded by a marker under the v2 state root named by the SHA-256 of
//! the project's canonical path ([`project_assets_marker`]). The marker lives
//! outside the repository and is written only for a project that had a v1
//! file.

use std::{
	collections::BTreeSet,
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use omp_core::{Hash32, StrMut};

use super::{AssetError, Entries, mcp, ssh, write};
use crate::v1_import::{
	Attention, ImportEntry, ImportError, ImportMode, ImportOutcome, ImportStep, SkipReason, V1Item,
	V1Layout, V1Source, V2Roots, step::atomic_replace,
};

/// The host files v1 also read at the project root, in its order.
const ROOT_SSH: [&str; 2] = ["ssh.json", ".ssh.json"];

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
		&& !ROOT_SSH.iter().any(|name| project.join(name).is_file())
		&& !omp.join(".mcp.json").is_file()
		&& !omp.join("commands").is_dir()
	{
		return single(ImportOutcome::NothingToImport);
	}
	let marker = project_assets_marker(project, roots);
	if inside_v1_root(&omp, source) {
		return import_home_hosts(project, source, roots, mode, &marker)
			.unwrap_or_else(|| single(ImportOutcome::Skipped(SkipReason::InsideV1Root)));
	}
	let layout = source.layout(None);
	if marker.exists() {
		return single(ImportOutcome::Skipped(SkipReason::MarkerPresent));
	}
	let mut entries = Vec::new();
	let converted = convert(&mut entries, project, &omp, &layout, mode).and_then(|()| {
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

/// The home directory as the project: its `.omp/` is the v1 install, which
/// is never written, but v1 still read the host files a project declares
/// there (`~/.omp/ssh.json`, then `~/ssh.json` and `~/.ssh.json`). They
/// convert into the active profile's user `hosts.toml`, in v1's order, once,
/// under the project's marker; everything else stays
/// [`SkipReason::InsideV1Root`]. `None` when the project is not the home
/// directory or has none of those files.
fn import_home_hosts(
	project: &Path,
	source: &V1Source,
	roots: &V2Roots,
	mode: ImportMode,
	marker: &Path,
) -> Option<Vec<ImportEntry>> {
	let layout = source.layout(roots.active_profile.as_deref());
	let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
	let omp = project.join(".omp");
	let found = |path: &Path| path.is_file();
	if canonical(project) != canonical(layout.home())
		|| !(found(&omp.join("ssh.json")) || ROOT_SSH.iter().any(|name| found(&project.join(name))))
	{
		return None;
	}
	let entry = |step, outcome| ImportEntry::new(step, V1Item::Ssh, Some(omp.clone()), outcome);
	if marker.exists() {
		return Some(vec![entry(
			ImportStep::SshHosts,
			ImportOutcome::Skipped(SkipReason::MarkerPresent),
		)]);
	}
	let hosts = roots
		.target(roots.active_profile.as_deref())
		.config_dir
		.join("hosts.toml");
	let mut out = Entries { step: ImportStep::SshHosts, mode, list: Vec::new() };
	let converted = convert_host_files(&mut out, project, &omp, &hosts, &layout).and_then(|()| {
		if mode == ImportMode::Apply {
			write_marker(marker, project)?;
		}
		Ok(())
	});
	let mut entries = out.list;
	for entry in &mut entries {
		entry.subject = entry.subject.take().map(|subject| {
			let mut text = StrMut::default();
			let _ = write!(text, "{subject} ({HOME_HOST})");
			text.freeze()
		});
	}
	if let Err(error) = converted {
		entries.push(entry(
			ImportStep::SshHosts,
			ImportOutcome::NeedsAttention(Attention::Failed(ImportError::Assets(error))),
		));
	}
	if omp.join(".mcp.json").is_file() || omp.join("commands").is_dir() {
		entries.push(entry(ImportStep::SshHosts, ImportOutcome::Skipped(SkipReason::InsideV1Root)));
	}
	Some(entries)
}

/// Why a host from the home directory's project files went to the user
/// hosts.
const HOME_HOST: &str = "imported as a user host: the project is your home directory";

/// Whether the project's `omp` directory lies in a v1 root: run from `$HOME`
/// (or inside a v1 root), the project's `.omp/` is the v1 install itself,
/// which is never written.
pub(in crate::v1_import) fn inside_v1_root(omp: &Path, source: &V1Source) -> bool {
	let layout = source.layout(None);
	let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
	let project_omp = canonical(omp);
	[source.base_root(), layout.agent_dir()]
		.into_iter()
		.any(|v1| project_omp.starts_with(canonical(v1)))
}

/// The three conversions, each reported under the user step it mirrors. The
/// v2 readers are `HostPaths::project`, `McpConfigPaths::project`, and
/// `PromptTemplates::discover`'s `<project>/.omp/prompts`.
fn convert(
	entries: &mut Vec<ImportEntry>,
	project: &Path,
	omp: &Path,
	layout: &V1Layout,
	mode: ImportMode,
) -> Result<(), AssetError> {
	let file = |name: &str| Some(omp.join(name)).filter(|path| path.is_file());
	let mut out = Entries { step: ImportStep::SshHosts, mode, list: Vec::new() };
	let result = convert_hosts(&mut out, project, omp, layout);
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

/// Converts every project host file v1 read into `.omp/hosts.toml`, in v1's
/// order. The v1 user `ssh.json` sits between `.omp/ssh.json` and the root
/// files: its hosts go to the user `hosts.toml` (the `ssh-hosts` step), and a
/// root-file host of the same name was never used by v1.
fn convert_hosts(
	out: &mut Entries,
	project: &Path,
	omp: &Path,
	layout: &V1Layout,
) -> Result<(), AssetError> {
	convert_host_files(out, project, omp, &omp.join("hosts.toml"), layout)
}

/// [`convert_hosts`] into `hosts`.
fn convert_host_files(
	out: &mut Entries,
	project: &Path,
	omp: &Path,
	hosts: &Path,
	layout: &V1Layout,
) -> Result<(), AssetError> {
	let home = layout.home();
	let present = |path: PathBuf| Some(path).filter(|path| path.is_file());
	let mut claimed = BTreeSet::new();
	ssh::convert(
		out,
		V1Item::Ssh,
		present(omp.join("ssh.json")),
		project,
		hosts,
		home,
		&mut claimed,
	)?;
	if let Some(user) = layout.locate(V1Item::Ssh) {
		claimed.extend(ssh::declared(&user));
	}
	for name in ROOT_SSH {
		if let Some(source) = present(project.join(name)) {
			ssh::convert(out, V1Item::Ssh, Some(source), project, hosts, home, &mut claimed)?;
		}
	}
	Ok(())
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
