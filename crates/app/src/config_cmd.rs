//! Reflected, typed settings command handlers.

use std::{
	env,
	path::{Path, PathBuf},
};

use miette::IntoDiagnostic as _;
use omp_con::{Ctx, DumpOptions, Origin, Source, ValueKind, VarFlags};
use omp_core::Str;
use omp_envd::mcp::{
	McpConfigPaths,
	config::McpServerConfig,
	config_store::{McpConfigStore, set_server_enabled},
	json_rpc,
};

use crate::cli::{ConfigCommand, ConfigScope, McpConfigCommand, McpConfigScope};

/// Runs a typed command-stream configuration operation.
pub fn run(command: &ConfigCommand) -> miette::Result<()> {
	let project = env::current_dir().into_diagnostic()?;
	if let ConfigCommand::ImportV1 { dry_run, from, profile, sessions } = command {
		return import_v1(&project, *dry_run, from.as_deref(), profile.as_deref(), *sessions);
	}
	if let ConfigCommand::Mcp { command } = command {
		let user_root = omp_core::dirs::user_config_root().into_diagnostic()?;
		return run_mcp(&user_root, &project, command);
	}
	match command {
		ConfigCommand::Dump => {
			print!("{}", crate::process_ctx(&project)?.dump());
			Ok(())
		},
		ConfigCommand::List { json } => list(&crate::process_ctx(&project)?, *json),
		ConfigCommand::Get { key } => get(&crate::process_ctx(&project)?, key),
		ConfigCommand::Set { key, value, scope } => set_persisted(&project, *scope, key, value),
		ConfigCommand::Unset { key, scope } => {
			let destination = path(&project, *scope)?;
			update_cfg(&destination, |ctx| {
				let omp_con::RegItem::Var(spec) = ctx
					.find(key)
					.ok_or_else(|| miette::miette!("unknown convar `{key}`; run `omp config list`"))?
				else {
					return Err(miette::miette!("`{key}` is not a convar"));
				};
				ctx.set(spec.name, (spec.default)(), Origin::Default)
					.into_diagnostic()?;
				Ok(())
			})
		},
		ConfigCommand::Path { scope } => {
			println!("{}", path(&project, *scope)?.display());
			Ok(())
		},
		ConfigCommand::Mcp { .. } => unreachable!("MCP commands return before config composition"),
		ConfigCommand::ImportV1 { .. } => unreachable!("v1 import returns before config"),
	}
}

/// `omp config import-v1`: locates the v1 install, pairs its profiles with
/// v2 profiles, runs every registered import step, imports the current
/// project's v1 settings and agents, and prints the report. Other projects
/// import their own the first time omp runs in them. `sessions` converts every
/// v1 session too, instead of leaving them to the resume picker.
fn import_v1(
	project: &Path,
	dry_run: bool,
	from: Option<&Path>,
	profile: Option<&str>,
	sessions: bool,
) -> miette::Result<()> {
	use omp_driver::v1_import::{
		CredentialAccess, ImportMode, ImportReport, ProfileSelection, SessionImport, V1Inputs,
		V1Source, V2Roots, import_project_agents, import_project_assets, import_project_settings,
		plan, run_with,
	};

	let mut inputs = V1Inputs::from_process()
		.ok_or_else(|| miette::miette!("HOME must be set to locate the v1 install"))?;
	if let Some(from) = from {
		if !from.is_dir() {
			return Err(miette::miette!("--from {} is not a directory", from.display()));
		}
		inputs = inputs.with_explicit_root(from.to_owned());
	}
	let source = V1Source::new(inputs);
	let selection = match profile {
		Some(profile) => {
			ProfileSelection::Named(omp_core::dirs::normalize_profile_name(profile).into_diagnostic()?)
		},
		None => ProfileSelection::All,
	};
	let sessions = if sessions {
		SessionImport::Bulk(&crate::session_import::V1Converter)
	} else {
		SessionImport::OnDemand
	};
	let mode = if dry_run {
		ImportMode::DryRun
	} else {
		ImportMode::Apply
	};
	let roots = V2Roots::from_process().into_diagnostic()?;
	let mut report = if !source.exists() {
		println!("no v1 install at {}; nothing to import", source.base_root().display());
		ImportReport { dry_run, ..ImportReport::default() }
	} else if let ProfileSelection::Named(Some(name)) = &selection
		&& !source.has_profile(name)
	{
		println!("no v1 profile {name} under {}; nothing to import", source.base_root().display());
		ImportReport { dry_run, ..ImportReport::default() }
	} else {
		let pairs = plan(&source, &roots, &selection).into_diagnostic()?;
		if dry_run {
			run_with(&pairs, mode, CredentialAccess::Offline(&Ctx::new()), sessions)
		} else {
			let ctx = crate::process_ctx(project)?;
			run_with(&pairs, mode, CredentialAccess::Offline(&ctx), sessions)
		}
	};
	report.project = import_project_settings(project, &roots, mode);
	report
		.project
		.extend(import_project_assets(project, &source, &roots, mode));
	report
		.project
		.extend(import_project_agents(project, &source, &roots, mode));
	print!("{}", render_v1_report(&report, project));
	let failed = report.entries().any(|entry| {
		matches!(
			entry.outcome,
			omp_driver::v1_import::ImportOutcome::NeedsAttention(
				omp_driver::v1_import::Attention::Failed(_)
			)
		)
	});
	if failed {
		return Err(miette::miette!("some v1 import steps failed; they retry on the next run"));
	}
	Ok(())
}

/// Renders a v1 import report for the terminal.
fn render_v1_report(report: &omp_driver::v1_import::ImportReport, project: &Path) -> String {
	use std::fmt::Write as _;

	fn profile(name: Option<&Str>) -> &str {
		name.map_or("default", Str::as_str)
	}

	let mut out = String::new();
	for pair in &report.pairs {
		let _ = writeln!(
			out,
			"v1 profile {} ({}) -> v2 profile {} ({})",
			profile(pair.source_profile.as_ref()),
			pair.source_root.display(),
			profile(pair.target_profile.as_ref()),
			pair.target_config.display(),
		);
		let _ = writeln!(out, "  agent dir    {}", pair.agent_dir.display());
		for (category, root) in &pair.xdg_roots {
			let category: &str = (*category).into();
			let _ = writeln!(out, "  xdg {category:<8} {}", root.display());
		}
		for collision in &pair.collisions {
			let _ = writeln!(
				out,
				"  warning: the v1 xdg {} root {} is shared with v2 ({}); it is left in place",
				collision.category,
				collision.v1.display(),
				collision.v2.display(),
			);
		}
		let _ = writeln!(out, "  inventory");
		if pair.inventory.is_empty() {
			let _ = writeln!(out, "    (nothing found)");
		}
		for (item, path, steps) in &pair.inventory {
			let item: &str = (*item).into();
			let _ = write!(out, "    {item:<24} {}", path.display());
			if steps.is_empty() {
				let _ = writeln!(out, "  (no importer yet)");
			} else {
				let _ = write!(out, "  <-");
				for step in steps {
					let _ = write!(out, " {step}");
				}
				out.push('\n');
			}
		}
		let _ = writeln!(
			out,
			"  {}",
			if report.dry_run {
				"import (dry run: nothing written)"
			} else {
				"import"
			}
		);
		for entry in &pair.entries {
			render_v1_entry(&mut out, entry);
		}
	}
	if !report.project.is_empty() {
		let _ = writeln!(
			out,
			"project {}{}",
			project.display(),
			if report.dry_run {
				" (dry run: nothing written)"
			} else {
				""
			}
		);
		for entry in &report.project {
			render_v1_entry(&mut out, entry);
		}
	}
	out
}

/// Renders one v1 import report entry.
fn render_v1_entry(out: &mut String, entry: &omp_driver::v1_import::ImportEntry) {
	use std::fmt::Write as _;

	use omp_driver::v1_import::{Attention, ImportOutcome};

	let kind: &str = entry.outcome.kind().into();
	let step: &str = entry.step.into();
	let _ = write!(out, "    {kind:<17} {step:<12}");
	if let Some(subject) = &entry.subject {
		let _ = write!(out, " {subject}");
	}
	if let Some(path) = &entry.path {
		let _ = write!(out, " {}", path.display());
	}
	match &entry.outcome {
		ImportOutcome::Skipped(reason) => {
			let _ = write!(out, ": {reason}");
		},
		ImportOutcome::NotMigratable(reason) => {
			let _ = write!(out, ": {reason}");
		},
		ImportOutcome::NeedsAttention(
			attention @ (Attention::Failed(error) | Attention::Incompatible(error)),
		) => {
			if matches!(attention, Attention::Incompatible(_)) {
				let _ = write!(out, ": {attention}");
			}
			let _ = write!(out, ": {error}");
			let mut source = std::error::Error::source(error);
			while let Some(cause) = source {
				let _ = write!(out, ": {cause}");
				source = cause.source();
			}
		},
		ImportOutcome::NeedsAttention(attention) => {
			let _ = write!(out, ": {attention}");
		},
		ImportOutcome::Imported
		| ImportOutcome::WouldImport
		| ImportOutcome::Reimported
		| ImportOutcome::WouldReimport
		| ImportOutcome::Removed
		| ImportOutcome::WouldRemove
		| ImportOutcome::NothingToImport => {},
	}
	out.push('\n');
}

fn run_mcp(user_root: &Path, project: &Path, command: &McpConfigCommand) -> miette::Result<()> {
	// The same three files `omp-envd` binds for `/mcp` mutations: the user
	// file lives in the `~/.o2` configuration root, never the data directory.
	let paths = McpConfigPaths::new(user_root, project);
	let user = McpConfigStore::new(paths.user);
	let project_store = McpConfigStore::new(paths.project);
	let root = McpConfigStore::new(paths.root);
	match command {
		McpConfigCommand::List { scope, json } => {
			let stores: Vec<(McpConfigScope, &McpConfigStore)> = match scope {
				Some(McpConfigScope::Global) => vec![(McpConfigScope::Global, &user)],
				Some(McpConfigScope::Project) => vec![(McpConfigScope::Project, &project_store)],
				Some(McpConfigScope::Root) => vec![(McpConfigScope::Root, &root)],
				None => vec![
					(McpConfigScope::Project, &project_store),
					(McpConfigScope::Global, &user),
					(McpConfigScope::Root, &root),
				],
			};
			if *json {
				let mut output = serde_json::Map::new();
				for (scope, store) in stores {
					for name in store.list().into_diagnostic()? {
						output
							.insert(name.to_string(), serde_json::json!({"scope": mcp_scope_name(scope)}));
					}
				}
				println!("{}", serde_json::to_string_pretty(&output).into_diagnostic()?);
			} else {
				for (scope, store) in stores {
					for name in store.list().into_diagnostic()? {
						println!("{}\t{name}", mcp_scope_name(scope));
					}
				}
			}
			Ok(())
		},
		McpConfigCommand::Get { name } => {
			for (scope, store) in [
				(McpConfigScope::Project, &project_store),
				(McpConfigScope::Global, &user),
				(McpConfigScope::Root, &root),
			] {
				if let Some(server) = store.get(name).into_diagnostic()? {
					println!(
						"{}",
						serde_json::to_string_pretty(&serde_json::json!({
							"name": name,
							"scope": mcp_scope_name(scope),
							"config": redacted_server(&server),
						}))
						.into_diagnostic()?
					);
					return Ok(());
				}
			}
			Err(miette::miette!("MCP server `{name}` was not found in native configuration"))
		},
		McpConfigCommand::Add { name, config, scope } => {
			let server: McpServerConfig = serde_json::from_str(config).into_diagnostic()?;
			mcp_store(*scope, &user, &project_store, &root)
				.add(name, server)
				.into_diagnostic()
		},
		McpConfigCommand::Update { name, config, scope } => {
			let server: McpServerConfig = serde_json::from_str(config).into_diagnostic()?;
			mcp_store(*scope, &user, &project_store, &root)
				.update(name, server)
				.into_diagnostic()
		},
		McpConfigCommand::Remove { name, scope } => mcp_store(*scope, &user, &project_store, &root)
			.remove(name)
			.into_diagnostic(),
		McpConfigCommand::Enable { name } | McpConfigCommand::Disable { name } => set_server_enabled(
			&user,
			&project_store,
			Some(&root),
			name,
			matches!(command, McpConfigCommand::Enable { .. }),
		)
		.into_diagnostic(),
	}
}

fn mcp_store<'a>(
	scope: McpConfigScope,
	user: &'a McpConfigStore,
	project: &'a McpConfigStore,
	root: &'a McpConfigStore,
) -> &'a McpConfigStore {
	match scope {
		McpConfigScope::Global => user,
		McpConfigScope::Project => project,
		McpConfigScope::Root => root,
	}
}

fn mcp_scope_name(scope: McpConfigScope) -> &'static str {
	scope.into()
}

fn redacted_server(server: &McpServerConfig) -> serde_json::Value {
	let mut value = serde_json::to_value(server).unwrap_or(serde_json::Value::Null);
	if let Some(url) = value.get_mut("url")
		&& let Some(raw) = url.as_str()
	{
		*url = serde_json::Value::String(json_rpc::redact_url_for_log(raw).to_string());
	}
	for map_name in ["env", "headers"] {
		if let Some(values) = value
			.get_mut(map_name)
			.and_then(serde_json::Value::as_object_mut)
		{
			for (name, value) in values {
				let name = name.to_ascii_lowercase();
				if ["key", "token", "secret", "authorization", "cookie"]
					.iter()
					.any(|needle| name.contains(needle))
				{
					*value = serde_json::Value::String("[REDACTED]".to_owned());
				}
			}
		}
	}
	value
}

/// Returns the selected command-stream configuration path.
pub fn path(project: &Path, scope: ConfigScope) -> miette::Result<PathBuf> {
	Ok(match scope {
		ConfigScope::Global => crate::config_path().into_diagnostic()?,
		ConfigScope::Project => project.join(".omp/config.cfg"),
	})
}

/// Loads an existing cfg leniently: lines this build no longer understands are
/// reported and dropped, so an edit never fails on a stale variable and the
/// re-dumped file no longer carries it.
pub(crate) fn load_cfg(path: &Path) -> miette::Result<Ctx> {
	let script = omp_driver::cfg::read_config(path).into_diagnostic()?;
	load_cfg_text(path, script.as_deref())
}

fn load_cfg_text(path: &Path, script: Option<&str>) -> miette::Result<Ctx> {
	let ctx = Ctx::new();
	// The default bind cfg is the baseline the persisted script diffs
	// against; without it a dump would `unbindall` the defaults away.
	ctx.exec(
		omp_driver::keybindings::DEFAULT_BINDS,
		Source::Config(Str::new_static(omp_driver::keybindings::DEFAULT_BINDS_NAME)),
	)
	.into_diagnostic()?;
	ctx.seal_bind_defaults();
	if let Some(script) = script {
		let outcome = ctx
			.exec_configs(&|name: &str| Ok((name == "config.cfg").then(|| Str::new(script))), None)
			.into_diagnostic()?;
		if outcome.failed > 0 {
			eprintln!(
				"warning: {} skipped {} statement(s) this build does not understand",
				path.display(),
				outcome.failed
			);
		}
	}
	Ok(ctx)
}

pub(crate) fn update_cfg(
	path: &Path,
	update: impl FnOnce(&Ctx) -> miette::Result<()>,
) -> miette::Result<()> {
	let transaction =
		omp_driver::cfg::ConfigFileLock::acquire(path.to_path_buf()).into_diagnostic()?;
	let current = transaction.read().into_diagnostic()?;
	let migrated = current
		.as_deref()
		.map(|script| omp_driver::cfg::migrate_config_script(path, script))
		.transpose()
		.into_diagnostic()?;
	let ctx = load_cfg_text(path, migrated.as_deref())?;
	update(&ctx)?;
	transaction
		.replace(
			ctx.dump_with_options(DumpOptions {
				include_archived_defaults: true,
				..DumpOptions::default()
			})
			.as_str(),
		)
		.into_diagnostic()
}

fn assignment(ctx: &Ctx, name: &str, input: &str) -> miette::Result<String> {
	let spec = ctx
		.vars()
		.find(|spec| spec.name.eq_ignore_ascii_case(name))
		.ok_or_else(|| miette::miette!("unknown convar `{name}`; run `omp config list`"))?;
	let value = if spec.ty.kind == ValueKind::Str {
		serde_json::to_string(input).into_diagnostic()?
	} else {
		input.to_owned()
	};
	Ok(format!("{name} {value}"))
}

fn list(ctx: &Ctx, json: bool) -> miette::Result<()> {
	let mut vars = ctx.vars().collect::<Vec<_>>();
	vars.sort_unstable_by_key(|spec| spec.name);
	if json {
		let mut output = serde_json::Map::new();
		for spec in vars {
			output.insert(
				spec.name.to_owned(),
				serde_json::json!({
					"value": ctx.value(spec.name).into_diagnostic()?.to_string(),
					"default": spec.default().to_string(),
					"flags": flag_names(spec.flags),
				}),
			);
		}
		println!("{}", serde_json::to_string_pretty(&output).into_diagnostic()?);
		return Ok(());
	}
	for spec in vars {
		println!(
			"{}\t{}\t{}\t{}",
			spec.name,
			ctx.value(spec.name).into_diagnostic()?,
			spec.default(),
			flag_names(spec.flags).join("|"),
		);
	}
	Ok(())
}

fn get(ctx: &Ctx, name: &str) -> miette::Result<()> {
	let value = ctx
		.value(name)
		.map_err(|_| miette::miette!("unknown convar `{name}`; run `omp config list`"))?;
	println!("{value}");
	Ok(())
}

fn flag_names(flags: VarFlags) -> Vec<&'static str> {
	[
		(VarFlags::ARCHIVE, "ARCHIVE"),
		(VarFlags::SESSION, "SESSION"),
		(VarFlags::REPLICATED, "REPLICATED"),
		(VarFlags::READONLY, "READONLY"),
		(VarFlags::NOTIFY, "NOTIFY"),
		(VarFlags::UNSAFE, "UNSAFE"),
	]
	.into_iter()
	.filter_map(|(flag, name)| flags.contains(flag).then_some(name))
	.collect()
}

/// Sets and persists one convar in the selected cfg scope.
pub fn set_persisted(
	project: &Path,
	scope: ConfigScope,
	name: &str,
	value: &str,
) -> miette::Result<()> {
	let destination = path(project, scope)?;
	update_cfg(&destination, |ctx| {
		let assignment = assignment(ctx, name, value)?;
		ctx.exec(&assignment, Source::Config(Str::new_static("config.cfg")))
			.into_diagnostic()?;
		Ok(())
	})
}
