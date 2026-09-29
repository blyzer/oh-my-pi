//! `/plugins`, `/marketplace`: marketplace sources and plugins over the
//! signed-index transactions shared with `omp ext`
//! ([`ExtensionTransactions`]). Network-bound operations (install,
//! catalog update, upgrade) run on the application runtime and settle a
//! [`Pending`] line the panel polls.

use std::fmt::Write as _;

use omp_chat::overlays::services::{
	BlockedCommandRow, MarketplaceSource, Pending, PluginRow, PluginsReport, ServiceError,
	ServiceResult,
};
use omp_core::{Str, sf};

use super::ServiceState;
use crate::ext_cli::{Scope, service::ExtensionTransactions};

fn transactions(state: &ServiceState) -> ExtensionTransactions {
	ExtensionTransactions::new(&state.data_dir, &state.project, Scope::User)
}

/// Runs a marketplace transaction on the runtime; the receiver settles
/// with its status line.
fn pending<F>(state: &ServiceState, work: F) -> Pending<Str>
where
	F: Future<Output = miette::Result<Str>> + Send + 'static,
{
	let (tx, rx) = flume::bounded(1);
	state.runtime.spawn(async move {
		let _ = tx.send(work.await.map_err(ServiceError::failed));
	});
	rx
}

/// Configured sources plus every plugin the catalogs offer, installed
/// entries first (with their registry state), then the rest.
pub(super) fn report(state: &ServiceState) -> ServiceResult<PluginsReport> {
	let transactions = transactions(state);
	let sources = transactions
		.indexes()
		.map_err(ServiceError::failed)?
		.into_iter()
		.map(|index| MarketplaceSource { name: index.name, uri: Str::new(index.url) })
		.collect::<Vec<_>>();
	let installed = transactions.installed().map_err(ServiceError::failed)?;
	let available = transactions.discover(None).map_err(ServiceError::failed)?;
	let mut plugins = Vec::with_capacity(installed.len() + available.len());
	for view in &installed {
		let marketplace = view.marketplace.clone().unwrap_or_default();
		let id = if marketplace.is_empty() {
			view.id.clone()
		} else {
			sf!("{}@{marketplace}", view.id)
		};
		let description = available
			.iter()
			.find(|package| package.id == view.id && package.marketplace == marketplace)
			.map(|package| package.description.clone())
			.unwrap_or_default();
		plugins.push(PluginRow {
			id,
			name: view.id.clone(),
			version: view.version.clone(),
			description,
			marketplace,
			installed: true,
			enabled: view.enabled,
			scope: Str::new_static(match view.scope {
				Scope::User => "user",
				Scope::Project => "project",
			}),
			shadowed: view.shadowed,
			external: false,
		});
	}
	// Claude Code's own installs load read-only beside omp's; list them so
	// the user sees what runs, never as something omp manages.
	let claude_code = omp_ext::claude_plugin::ClaudePlugins::resolve(
		&state.data_dir,
		&state.project,
		omp_ext::claude_plugin::ClaudeCodeHome::detect().as_ref(),
	);
	for plugin in claude_code
		.plugins
		.into_iter()
		.filter(|plugin| plugin.source == omp_ext::claude_plugin::PluginSource::ClaudeCode)
	{
		if plugins.iter().any(|row| row.id == plugin.id.as_str()) {
			continue;
		}
		plugins.push(PluginRow {
			id:          plugin.id.into(),
			name:        plugin.name,
			version:     Some(plugin.version),
			description: Str::default(),
			marketplace: plugin.marketplace,
			installed:   true,
			enabled:     true,
			scope:       Str::new_static(plugin.scope.into()),
			shadowed:    false,
			external:    true,
		});
	}
	for package in available {
		let already = installed.iter().any(|view| {
			view.id == package.id && view.marketplace.as_deref() == Some(package.marketplace.as_str())
		}) || plugins.iter().any(|row| {
			row.external && row.name == package.id && row.marketplace == package.marketplace
		});
		if already {
			continue;
		}
		plugins.push(PluginRow {
			id:          sf!("{}@{}", package.id, package.marketplace),
			name:        package.id,
			version:     Some(package.version),
			description: package.description,
			marketplace: package.marketplace,
			installed:   false,
			enabled:     false,
			scope:       Str::default(),
			shadowed:    false,
			external:    false,
		});
	}
	Ok(PluginsReport {
		marketplaces: sources.iter().map(|source| source.name.clone()).collect(),
		plugins,
		sources,
	})
}

/// Installs `name@marketplace`; settles `Installed <name> from <marketplace>`.
pub(super) fn install(state: &ServiceState, id: &str) -> ServiceResult<Pending<Str>> {
	let transactions = transactions(state);
	let spec = id.to_owned();
	Ok(pending(state, async move {
		let package = transactions.install(&spec, false).await?;
		Ok(sf!("Installed {} from {}", package.id, package.marketplace))
	}))
}

/// Uninstalls `name@marketplace` (or a unique bare name); settles
/// `Uninstalled <id>`.
pub(super) fn uninstall(state: &ServiceState, id: &str) -> ServiceResult<Pending<Str>> {
	let transactions = transactions(state);
	let spec = id.to_owned();
	Ok(pending(state, async move {
		let removed = transactions.uninstall(&spec)?;
		Ok(sf!("Uninstalled {removed}"))
	}))
}

/// Flips an installed plugin's enabled flag in the user-scope registry.
pub(super) fn set_enabled(state: &ServiceState, id: &str, enabled: bool) -> ServiceResult<()> {
	transactions(state)
		.set_enabled(id, enabled)
		.map(|_| ())
		.map_err(ServiceError::failed)
}

/// Fetches and registers a marketplace catalog; returns `Added marketplace:
/// <name>`. The fetch may clone a repository, so it blocks the calling
/// thread only through the runtime's blocking bridge.
pub(super) fn add_marketplace(state: &ServiceState, source: &str) -> ServiceResult<Str> {
	let transactions = transactions(state);
	let source = source.to_owned();
	let handle = state.runtime.clone();
	let added = std::thread::scope(|scope| {
		scope
			.spawn(|| handle.block_on(transactions.add_index(&source)))
			.join()
			.map_err(|_| ServiceError::Failed(Str::new_static("marketplace fetch panicked")))
	})?
	.map_err(ServiceError::failed)?;
	Ok(sf!("Added marketplace: {}", added.name))
}

/// Removes a marketplace source and its cache; returns `Removed
/// marketplace: <name>`.
pub(super) fn remove_marketplace(state: &ServiceState, name: &str) -> ServiceResult<Str> {
	transactions(state)
		.remove_index(name)
		.map_err(ServiceError::failed)?;
	Ok(sf!("Removed marketplace: {name}"))
}

/// Re-fetches one or every catalog; settles `Updated marketplace: <name>`
/// or `Updated N marketplace(s)`.
pub(super) fn update_marketplace(
	state: &ServiceState,
	name: Option<&str>,
) -> ServiceResult<Pending<Str>> {
	let transactions = transactions(state);
	let name = name.map(str::to_owned);
	Ok(pending(state, async move {
		let updated = transactions.update_index(name.as_deref()).await?;
		Ok(match name {
			Some(name) => sf!("Updated marketplace: {name}"),
			None => sf!("Updated {} marketplace(s)", updated.len()),
		})
	}))
}

/// Upgrades one or every outdated plugin; settles `Upgraded …` lines
/// or `All marketplace plugins are up to date`.
pub(super) fn upgrade(state: &ServiceState, spec: Option<&str>) -> ServiceResult<Pending<Str>> {
	let transactions = transactions(state);
	let spec = spec.map(str::to_owned);
	Ok(pending(state, async move {
		let upgrades = transactions.upgrade(spec.as_deref()).await?;
		if let Some(spec) = spec {
			let version = upgrades
				.first()
				.and_then(|upgrade| upgrade.to.clone())
				.unwrap_or_else(|| Str::new_static("the current version"));
			return Ok(sf!("Upgraded {spec} to {version}"));
		}
		if upgrades.is_empty() {
			return Ok(Str::new_static("All marketplace plugins are up to date"));
		}
		let mut lines = format!("Upgraded {} plugin(s):", upgrades.len());
		for upgrade in &upgrades {
			lines.push_str(&format!(
				"\n  {}: {} -> {}",
				upgrade.id,
				upgrade.from.as_deref().unwrap_or("?"),
				upgrade.to.as_deref().unwrap_or("?")
			));
		}
		Ok(Str::new(lines))
	}))
}

/// `/plugins approve`: every plugin command a session in this project, with
/// this invocation's plugin roots, would not start, read fresh from the
/// grant file, so a command approved since drops out.
pub(super) fn blocked_commands(state: &ServiceState) -> ServiceResult<Vec<BlockedCommandRow>> {
	let blocked = omp_driver::plugin_commands::session_blocked_launches(
		&state.data_dir,
		&state.project,
		&state.plugin_dirs,
		&omp_envd::mcp::McpSettings::from_con(&state.con),
	)
	.map_err(ServiceError::failed)?;
	Ok(blocked
		.into_iter()
		.map(|blocked| BlockedCommandRow {
			kind:       Str::new_static(blocked.kind.into()),
			server:     blocked.server.clone(),
			command:    sf!("{}", blocked.command_line()),
			digest:     blocked.digest,
			unreadable: blocked
				.unreadable
				.as_ref()
				.map(|unreadable| Str::new(unreadable.path.to_string_lossy())),
			plugin:     blocked.plugin.into(),
		})
		.collect())
}

/// `/plugins approve <plugin> <digest>|all`: records the operator's approval
/// through the same admission `omp ext trust --approve-command(s)` uses
/// ([`omp_driver::plugin_commands::approve_commands`]): the same digest, the
/// same grant file, and the same refusal of a command naming an unreadable
/// plugin file. The running session loaded its plugins at launch, so an
/// approved command starts after `/restart`.
pub(super) fn approve_commands(
	state: &ServiceState,
	plugin: &str,
	digest: Option<omp_core::Hash32>,
) -> ServiceResult<Str> {
	use omp_driver::plugin_commands::{CommandSelection, approve_commands, resolve_command_sets};
	use omp_ext::plugin_command::PluginId;

	let plugin = PluginId::from_ref(plugin);
	let (sets, _) =
		resolve_command_sets(&state.data_dir, &state.project, &state.plugin_dirs, plugin)
			.map_err(ServiceError::failed)?;
	let selection = match &digest {
		Some(digest) => CommandSelection::Digests(std::slice::from_ref(digest)),
		None => CommandSelection::All,
	};
	let outcome =
		approve_commands(&state.data_dir, &sets, plugin, selection, Str::new_static("interactive"))
			.map_err(ServiceError::failed)?;
	let mut line = String::new();
	for (id, launch) in &outcome.approved {
		let _ = write!(line, "Approved {id} {} `{}`. ", launch.kind, launch.server);
	}
	for (id, launch) in &outcome.unreadable {
		if let Some(unreadable) = launch.files.unreadable() {
			let _ = write!(
				line,
				"Not approved {id} {} `{}`: the plugin file `{}` it names cannot be read. ",
				launch.kind,
				launch.server,
				unreadable.path.display()
			);
		}
	}
	if outcome.approved.is_empty() {
		if outcome.unreadable.is_empty() {
			let _ = write!(line, "{plugin} launches no commands.");
		}
	} else {
		line.push_str("Run /restart to load the approved commands.");
	}
	Ok(Str::new(line.trim_end()))
}
