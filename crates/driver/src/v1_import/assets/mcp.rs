//! v1 MCP documents merged into a v2 `mcp.json`.
//!
//! v1 and v2 share the document shape (`mcpServers`, `disabledServers`,
//! `enabledServers`; v2 even keeps v1's schema URL), so each v1 declaration
//! parses into [`McpConfigFile`] as is. The merge is a copy: v1 files are
//! read, never passed to `McpConfigStore::migrate_from`, which deletes its
//! source. A name v2 already declares keeps v2's declaration.

use std::{
	fs,
	path::{Path, PathBuf},
};

use omp_core::Str;
use omp_envd::mcp::{
	config::{McpConfigFile, validate_server, validate_server_name},
	config_store::McpConfigStore,
};

use super::{AssetError, Entries};
use crate::v1_import::{
	ImportMode, ImportOutcome, V1Item,
	report::{Attention, SkipReason},
};

/// Merges `sources`, in v1's precedence order, into `destination`.
pub(super) fn merge(
	out: &mut Entries,
	item: V1Item,
	sources: &[PathBuf],
	root: &Path,
	destination: &Path,
) -> Result<(), AssetError> {
	if sources.is_empty() {
		out.push(item, None, None, ImportOutcome::NothingToImport);
		return Ok(());
	}
	let store = McpConfigStore::new(destination.to_owned());
	let mut merged = store.read().map_err(AssetError::McpStore)?;
	let mut changed = false;
	for source in sources {
		let bytes = fs::read(source)
			.map_err(|error| AssetError::Read { path: source.clone(), source: error })?;
		let file = match serde_json::from_slice::<McpConfigFile>(&bytes) {
			Ok(file) => file,
			Err(error) => {
				out.push(
					item,
					Some(source.clone()),
					Some(super::relative(root, destination)),
					ImportOutcome::NeedsAttention(Attention::Incompatible(
						AssetError::McpDocument(error).into(),
					)),
				);
				continue;
			},
		};
		let before = out.list.len();
		for (name, server) in file.mcp_servers {
			let issue = validate_server_name(&name)
				.err()
				.or_else(|| validate_server(&name, &server).into_iter().next());
			let outcome = if let Some(issue) = issue {
				ImportOutcome::NeedsAttention(Attention::Incompatible(
					AssetError::McpServer(issue).into(),
				))
			} else {
				match merged.mcp_servers.get(&name) {
					Some(existing) if *existing == server => {
						ImportOutcome::Skipped(SkipReason::AlreadyPresent)
					},
					Some(_) => ImportOutcome::NeedsAttention(Attention::Conflict),
					None => {
						merged.mcp_servers.insert(name.clone(), server);
						changed = true;
						out.copied()
					},
				}
			};
			out.push(item, Some(source.clone()), Some(name), outcome);
		}
		for (list, names, disabled) in [
			("disabledServers", file.disabled_servers, true),
			("enabledServers", file.enabled_servers, false),
		] {
			for name in names {
				let (own, opposite) = if disabled {
					(&mut merged.disabled_servers, &merged.enabled_servers)
				} else {
					(&mut merged.enabled_servers, &merged.disabled_servers)
				};
				let outcome = if own.contains(&name) {
					ImportOutcome::Skipped(SkipReason::AlreadyPresent)
				} else if opposite.contains(&name) {
					ImportOutcome::NeedsAttention(Attention::Conflict)
				} else {
					own.insert(name.clone());
					changed = true;
					out.copied()
				};
				let subject = Str::from(format_args!("{list} {name}"));
				out.push(item, Some(source.clone()), Some(subject), outcome);
			}
		}
		if out.list.len() == before {
			out.push(item, Some(source.clone()), None, ImportOutcome::NothingToImport);
		}
	}
	if changed && out.mode == ImportMode::Apply {
		store.write(&merged).map_err(AssetError::McpStore)?;
	}
	Ok(())
}
