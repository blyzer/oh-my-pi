//! Durable project-scoped model-role assignments.

use std::env;

use omp_catalog::{
	ModelKey, ModelRole, SelectedModel, SelectionError, parse_selector, select_model,
	settings::ModelSettings, snapshot::Catalog,
};
use omp_core::Str;

/// Environment override of the remembered default model.
const DEFAULT_MODEL_ENV: &str = "OMP_DEFAULT_MODEL";

/// Invocation-local resolved auxiliary model roles.
///
/// CLI values outrank the environment. The default model is not among them:
/// it resolves through [`resolve_launch_default`] against the catalog the
/// session routes through.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LaunchRoles {
	/// Fast/low-cost model.
	pub smol:          Option<ModelKey>,
	/// Fast selector's explicit thinking annotation.
	pub smol_thinking: Option<Str>,
	/// Deep-reasoning model.
	pub slow:          Option<ModelKey>,
	/// Deep selector's explicit thinking annotation.
	pub slow_thinking: Option<Str>,
	/// Planning model.
	pub plan:          Option<ModelKey>,
	/// Planning selector's explicit thinking annotation.
	pub plan_thinking: Option<Str>,
}

/// The remembered default model as one catalog resolves it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchDefault {
	/// The remembered selector resolved.
	Resolved(SelectedModel),
	/// The remembered selector names no model this catalog lists.
	Missing {
		/// The remembered selector, verbatim.
		selector: Str,
	},
	/// Nothing is remembered.
	Unset,
}

/// Resolves the remembered default model: `OMP_DEFAULT_MODEL`, else the
/// `default` role of `ai_model_roles` — the one persisted default (`ai_model`
/// is the live session route and is never archived).
///
/// A provider-qualified selector, the form the model picker persists, is an
/// identity: it resolves only to the exact model or alias it names. A fuzzy
/// match on some other model is reported as [`LaunchDefault::Missing`] rather
/// than substituted silently. Bare patterns and `@role` references keep
/// catalog selection's matching.
pub fn resolve_launch_default(catalog: &Catalog, settings: &ModelSettings) -> LaunchDefault {
	let environment = env::var(DEFAULT_MODEL_ENV)
		.ok()
		.filter(|value| !value.trim().is_empty());
	let Some(selector) = environment
		.as_deref()
		.or_else(|| settings.role_selector("default").map(Str::as_str))
	else {
		return LaunchDefault::Unset;
	};
	let missing = || LaunchDefault::Missing { selector: Str::new(selector) };
	let Ok(selected) = resolve_role_selector(catalog, settings, selector) else {
		return missing();
	};
	if !selector.starts_with('@') && selector.contains('/') {
		let named = parse_selector(selector)
			.ok()
			.map(|parsed| parsed.model)
			.into_iter()
			.chain([Str::new(selector)])
			.find_map(|name| {
				catalog
					.model(ModelKey::from_ref(name.as_str()))
					.or_else(|| catalog.resolve_alias(name.as_str()))
					.map(|model| model.key.clone())
			});
		if named.as_ref() != Some(&selected.model) {
			return missing();
		}
	}
	LaunchDefault::Resolved(selected)
}

/// Resolves auxiliary role selectors through the catalog authority. CLI
/// values override `OMP_*_MODEL`; unsupported thinking annotations are
/// rejected by catalog selection rather than clamped client-side.
pub fn resolve_launch_roles(
	catalog: &Catalog,
	settings: &ModelSettings,
	smol: Option<&str>,
	slow: Option<&str>,
	plan: Option<&str>,
) -> Result<LaunchRoles, SelectionError> {
	let configured_roles = configured_roles(settings)?;
	let models = eligible_models(catalog, settings);
	let resolve_selected = |cli: Option<&str>, variable: &str, role: &str| {
		let environment = env::var(variable).ok();
		let Some(selector) = cli
			.or(environment.as_deref())
			.or_else(|| settings.role_selector(role).map(Str::as_str))
		else {
			return Ok(None);
		};
		select_model(
			&models,
			catalog.routes(),
			catalog.aliases(),
			&configured_roles,
			&Default::default(),
			selector,
		)
		.map(Some)
	};
	let smol = resolve_selected(smol, "OMP_SMOL_MODEL", "smol")?;
	let slow = resolve_selected(slow, "OMP_SLOW_MODEL", "slow")?;
	let plan = resolve_selected(plan, "OMP_PLAN_MODEL", "plan")?;
	Ok(LaunchRoles {
		smol_thinking: smol.as_ref().and_then(|selected| selected.thinking.clone()),
		smol:          smol.map(|selected| selected.model),
		slow_thinking: slow.as_ref().and_then(|selected| selected.thinking.clone()),
		slow:          slow.map(|selected| selected.model),
		plan_thinking: plan.as_ref().and_then(|selected| selected.thinking.clone()),
		plan:          plan.map(|selected| selected.model),
	})
}
/// Resolves one explicit role selector through the catalog authority with no
/// environment fallback (e.g. `--plan-yolo-into`, `--prewalk-into`).
pub fn resolve_role_selector(
	catalog: &Catalog,
	settings: &ModelSettings,
	selector: &str,
) -> Result<SelectedModel, SelectionError> {
	let roles = configured_roles(settings)?;
	let models = eligible_models(catalog, settings);
	select_model(&models, catalog.routes(), catalog.aliases(), &roles, &Default::default(), selector)
}

fn configured_roles(settings: &ModelSettings) -> Result<Vec<ModelRole>, SelectionError> {
	let mut roles = settings
		.roles
		.iter()
		.map(|(id, selector)| {
			let mut role = ModelRole::assignment(id.clone(), selector.as_str(), None)?;
			if let Some(tag) = settings.role_tag(id) {
				role.display_name = Some(tag.name.clone());
				role.color = tag.color.clone();
				role.hidden = tag.hidden;
			}
			role.cycle_order = settings
				.cycle_order
				.iter()
				.position(|candidate| candidate == id)
				.and_then(|index| u32::try_from(index).ok());
			role.provider_rank = settings
				.provider_order
				.iter()
				.cloned()
				.map(omp_catalog::ProviderId::from)
				.collect::<Vec<_>>()
				.into_boxed_slice();
			Ok(role)
		})
		.collect::<Result<Vec<_>, SelectionError>>()?;
	roles = omp_catalog::known_roles(&roles);
	Ok(roles)
}

/// Reports whether one concrete selector remains inside configured model and
/// provider admission.
pub fn model_selector_allowed(catalog: &Catalog, settings: &ModelSettings, selector: &str) -> bool {
	catalog
		.model(ModelKey::from_ref(selector))
		.or_else(|| catalog.resolve_alias(selector))
		.is_some_and(|model| {
			model.routes.iter().any(|route_id| {
				catalog.route(route_id).is_some_and(|route| {
					let model_id = model
						.key
						.as_str()
						.split_once('/')
						.map_or(model.key.as_str(), |(_, model)| model);
					settings.model_allowed(route.provider.as_str(), model_id)
				})
			})
		})
}

/// Chooses the deterministic allowed fallback model.
pub fn fallback_model_selector(catalog: &Catalog, settings: &ModelSettings) -> Option<Str> {
	let models = eligible_models(catalog, settings);
	let mru = Default::default();
	omp_catalog::find_smol(&models, catalog.routes(), &mru)
		.or_else(|| omp_catalog::pick_default(&models, catalog.routes(), &mru))
		.map(|selected| Str::new(selected.model.as_str()))
}

fn eligible_models(catalog: &Catalog, settings: &ModelSettings) -> Vec<omp_catalog::ModelSpec> {
	catalog
		.models()
		.iter()
		.filter(|model| {
			model.routes.iter().any(|route_id| {
				catalog.route(route_id).is_some_and(|route| {
					let model_id = model
						.key
						.as_str()
						.split_once('/')
						.map_or(model.key.as_str(), |(_, model)| model);
					settings.model_allowed(route.provider.as_str(), model_id)
				})
			})
		})
		.cloned()
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn explicit_auto_survives_role_snapshot_codec() {
		let roles = vec![
			ModelRole::assignment("default", "openai/primary", Some("high")).expect("default role"),
			ModelRole::assignment("task", "openai-codex/worker", Some("auto")).expect("task role"),
		];
		let encoded = serde_json::to_vec(&roles).expect("encode role snapshot");
		let decoded: Vec<ModelRole> = serde_json::from_slice(&encoded).expect("decode role snapshot");
		assert_eq!(decoded, roles);
		assert_eq!(decoded[1].selectors[0].as_str(), "openai-codex/worker:auto");
	}
	#[test]
	fn configured_role_and_model_admission_drive_launch_resolution() {
		let catalog = omp_catalog::snapshot::Catalog::try_embedded().expect("catalog");
		let model = catalog.models().first().expect("model");
		let mut settings = ModelSettings::default();
		settings
			.roles
			.insert(Str::new_static("default"), Str::new(model.key.as_str()));
		let LaunchDefault::Resolved(launch) = resolve_launch_default(catalog, &settings) else {
			panic!("the default role resolves");
		};
		assert_eq!(launch.model, model.key);
		let provider = catalog
			.route(model.routes.first().expect("route"))
			.expect("route")
			.provider
			.clone();
		settings.disabled_providers =
			[omp_catalog::settings::PathScopedStringEntry::Bare(Str::new(provider.as_str()))].into();
		assert_eq!(resolve_launch_default(catalog, &settings), LaunchDefault::Missing {
			selector: Str::new(model.key.as_str()),
		});
	}

	#[test]
	fn a_qualified_default_the_catalog_does_not_list_is_missing_not_substituted() {
		let catalog = omp_catalog::snapshot::Catalog::try_embedded().expect("catalog");
		let model = catalog.models().first().expect("model");
		let unlisted = format!("{}-discovered-elsewhere", model.key);
		let mut settings = ModelSettings::default();
		settings
			.roles
			.insert(Str::new_static("default"), Str::new(unlisted.as_str()));
		assert_eq!(resolve_launch_default(catalog, &settings), LaunchDefault::Missing {
			selector: Str::new(unlisted.as_str()),
		});
		settings.roles.clear();
		assert_eq!(resolve_launch_default(catalog, &settings), LaunchDefault::Unset);
	}

	/// The precedence rule: `ai_model_roles.default` is the one persisted
	/// default model and `ai_model` is the session's live route. A picker
	/// choice saved to `config.cfg` can therefore never outrank a later
	/// default-role assignment on the next start.
	#[test]
	fn the_default_role_is_the_only_persisted_default_model() {
		let saved = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
		let sink = std::sync::Arc::clone(&saved);
		let ctx = omp_con::Ctx::builder()
			.saver(move |_, contents| {
				contents.clone_into(&mut sink.lock());
				Ok(())
			})
			.build();
		for line in ["ai_model picked/model", "ai_model_roles {default assigned/model}", "writecfg"] {
			ctx.exec(line, omp_con::Source::Console).expect(line);
		}
		let archive = saved.lock().clone();
		assert!(archive.contains("ai_model_roles"), "the default role persists:\n{archive}");
		assert!(
			!archive.lines().any(|line| line.starts_with("ai_model ")),
			"the live route is never archived:\n{archive}"
		);

		let next = omp_con::Ctx::new();
		next
			.exec(&archive, omp_con::Source::Config(Str::new_static("config.cfg")))
			.expect("replay the saved config");
		assert!(omp_agent::AI_MODEL.get(&next).is_empty(), "the next start has no live route");
		let settings = ModelSettings::from_con(&next);
		assert_eq!(settings.role_selector("default").map(Str::as_str), Some("assigned/model"));
	}

	#[test]
	fn every_launch_role_preserves_its_explicit_thinking_annotation() {
		let catalog = omp_catalog::snapshot::Catalog::try_embedded().expect("catalog");
		let settings = ModelSettings::default();
		let selector = "openai/gpt-5:high";
		let launch =
			resolve_launch_roles(catalog, &settings, Some(selector), Some(selector), Some(selector))
				.expect("annotated roles");
		for thinking in [launch.smol_thinking, launch.slow_thinking, launch.plan_thinking] {
			assert_eq!(thinking.as_deref(), Some("high"));
		}
	}

	#[test]
	fn configured_role_aliases_reach_recursive_catalog_resolution() {
		let catalog = omp_catalog::snapshot::Catalog::try_embedded().expect("catalog");
		let mut settings = ModelSettings::default();
		settings
			.roles
			.insert(Str::new_static("task"), Str::new_static("@slow"));
		settings
			.roles
			.insert(Str::new_static("default"), Str::new_static("@task"));
		assert!(matches!(resolve_launch_default(catalog, &settings), LaunchDefault::Resolved(_)));
	}
}
