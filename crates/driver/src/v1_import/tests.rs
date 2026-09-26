//! Locator, pairing, and framework proofs over temporary homes, v1 trees,
//! and v2 roots. Nothing here reads the process environment or the real
//! `~/.omp` / `~/.o2`.

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
	sync::Arc,
};

use omp_ai::{
	account::AccountPool,
	auth::{AuthControlHandle, CredentialStore, HeadlessKeySource, KeyId},
};
use omp_catalog::ProviderId;

use super::*;
use crate::discovery::models::{ModelsConfigLocation, load_or_import_legacy};

const V1_MODELS_YML: &str = concat!(
	"providers:\n",
	"  easycliproxy:\n",
	"    baseUrl: https://proxy.example/v1\n",
	"    apiKey: sk-literal-key\n",
	"    models:\n",
	"      - id: claude-opus-5\n",
	"  envkey:\n",
	"    baseUrl: https://env.example/v1\n",
	"    apiKey: EASY_PROXY_KEY\n",
);

fn write(path: &Path, contents: &str) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
	fs::write(path, contents).expect("write");
}

fn inputs(home: &Path) -> V1Inputs {
	V1Inputs { home: home.to_owned(), ..V1Inputs::default() }
}

/// v2 roots under the scratch root, like `~/.o2` and `~/.local/share/omp`.
fn roots(root: &Path, active: Option<&str>) -> V2Roots {
	V2Roots {
		config_dir:     root.join("o2"),
		data_dir:       root.join("share/omp"),
		state_dir:      root.join("state/omp"),
		cache_dir:      root.join("cache/omp"),
		active_profile: active.map(omp_core::Str::new),
	}
}

/// Every file (with its bytes) and directory under `root`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
	let mut tree = BTreeMap::new();
	let mut pending = vec![root.to_owned()];
	while let Some(directory) = pending.pop() {
		let Ok(entries) = fs::read_dir(&directory) else {
			continue;
		};
		for entry in entries {
			let path = entry.expect("entry").path();
			if path.is_dir() {
				tree.insert(path.clone(), None);
				pending.push(path);
			} else {
				tree.insert(path.clone(), Some(fs::read(&path).expect("read")));
			}
		}
	}
	tree
}

/// A control handle over a throwaway encrypted store.
fn control(root: &Path) -> (AuthControlHandle, Arc<CredentialStore>) {
	let store = Arc::new(
		CredentialStore::open(
			root.join("credentials.sqlite"),
			Arc::new(HeadlessKeySource::new(KeyId::new("v1-import-test"), [0x44; 32])),
		)
		.expect("store"),
	);
	let catalog = Arc::new(omp_catalog::Catalog::embedded().clone());
	let control =
		AuthControlHandle::offline(catalog, Arc::clone(&store), AccountPool::new()).expect("control");
	(control, store)
}

fn outcomes(report: &ImportReport) -> Vec<(ImportStep, Option<&str>, OutcomeKind)> {
	report
		.entries()
		.map(|entry| (entry.step, entry.subject.as_deref(), entry.outcome.kind()))
		.collect()
}

#[test]
fn the_default_layout_resolves_every_item_as_v1_does() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let omp = home.join(".omp");
	let agent = omp.join("agent");
	write(&agent.join("config.yaml"), "theme: dark\n");
	write(&agent.join("models.yml"), V1_MODELS_YML);
	write(&agent.join("agent.db"), "db");
	write(&agent.join("sessions/a.jsonl"), "{}");
	write(&agent.join(".mcp.json"), "{}");
	write(&agent.join("mcp.json"), "{}");
	write(&agent.join(".lsp.yaml"), "{}");
	write(&agent.join("secret-placeholder.key"), "key");
	write(&agent.join("memories/mnemopi/mnemopi.db"), "m");
	write(&omp.join("install-id"), "00000000-0000-0000-0000-000000000000\n");
	write(&omp.join("marketplaces.json"), "{}");
	write(&omp.join("plugins/installed_plugins.json"), "{}");

	let layout = V1Source::new(inputs(&home)).layout(None);
	assert_eq!(layout.config_root(), omp);
	assert_eq!(layout.agent_dir(), agent);
	assert_eq!(layout.xdg_root(XdgCategory::Data), None);
	// `config.yml` first, `config.yaml` as the fallback.
	assert_eq!(layout.locate(V1Item::Settings), Some(agent.join("config.yaml")));
	write(&agent.join("config.yml"), "theme: light\n");
	assert_eq!(layout.locate(V1Item::Settings), Some(agent.join("config.yml")));
	// `mcp.json` outranks `.mcp.json`; LSP takes any of its six spellings.
	assert_eq!(layout.locate(V1Item::Mcp), Some(agent.join("mcp.json")));
	assert_eq!(layout.locate(V1Item::Lsp), Some(agent.join(".lsp.yaml")));
	assert_eq!(layout.locate(V1Item::Sessions), Some(agent.join("sessions")));
	assert_eq!(layout.locate(V1Item::MnemopiMemory), Some(agent.join("memories/mnemopi")));
	assert_eq!(layout.locate(V1Item::InstallId), Some(omp.join("install-id")));
	assert_eq!(layout.locate(V1Item::Marketplaces), Some(omp.join("marketplaces.json")));
	assert_eq!(layout.locate(V1Item::Plugins), Some(omp.join("plugins")));
	// A directory item never matches a file, nor a file item a directory.
	write(&agent.join("skills"), "not a directory");
	assert_eq!(layout.locate(V1Item::Skills), None);
	assert_eq!(layout.locate(V1Item::Blobs), None);

	let found = layout.inventory().map(|(item, _)| item).collect::<Vec<_>>();
	assert_eq!(found, [
		V1Item::Settings,
		V1Item::Models,
		V1Item::AgentDb,
		V1Item::Sessions,
		V1Item::Mcp,
		V1Item::Lsp,
		V1Item::SecretPlaceholderKey,
		V1Item::InstallId,
		V1Item::MnemopiMemory,
		V1Item::Marketplaces,
		V1Item::Plugins,
	]);
}

#[test]
fn pi_coding_agent_dir_replaces_only_the_default_profiles_agent_dir() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let custom = root.path().join("custom-agent");
	let source = V1Source::new(V1Inputs { agent_dir: Some(custom.clone()), ..inputs(&home) });
	assert_eq!(source.layout(None).agent_dir(), custom);
	// The config root stays `~/.omp`, so root items do not follow the agent dir.
	assert_eq!(source.layout(None).config_root(), home.join(".omp"));
	// A named profile always derives its own agent dir.
	assert_eq!(source.layout(Some("work")).agent_dir(), home.join(".omp/profiles/work/agent"));
	// An empty value is unset.
	let source = V1Source::new(V1Inputs { agent_dir: Some(PathBuf::new()), ..inputs(&home) });
	assert_eq!(source.layout(None).agent_dir(), home.join(".omp/agent"));
	// A value `PI_PROFILE` derived (propagated by a parent's `setProfile`) is
	// not a default-profile override.
	let derived = home.join(".omp/profiles/work/agent");
	let source = V1Source::new(V1Inputs {
		agent_dir: Some(derived),
		omp_profile: Some("".into()),
		pi_profile: Some("work".into()),
		..inputs(&home)
	});
	assert_eq!(source.active_profile(), None, "an empty OMP_PROFILE selects the default");
	assert_eq!(source.layout(None).agent_dir(), home.join(".omp/agent"));
	// Nor is one derived for v1's own selected profile.
	let source = V1Source::new(V1Inputs {
		agent_dir: Some(home.join(".omp/profiles/work/agent")),
		omp_profile: Some("work".into()),
		..inputs(&home)
	});
	assert_eq!(source.layout(None).agent_dir(), home.join(".omp/agent"));
}

#[test]
fn pi_config_dir_renames_the_root_under_home() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let source = V1Source::new(V1Inputs { config_dir: Some(".omp-alt".into()), ..inputs(&home) });
	assert_eq!(source.base_root(), home.join(".omp-alt"));
	assert_eq!(source.layout(None).agent_dir(), home.join(".omp-alt/agent"));
	assert_eq!(source.layout(Some("work")).agent_dir(), home.join(".omp-alt/profiles/work/agent"));
	// Node's `path.join` keeps even an absolute name under home.
	let source = V1Source::new(V1Inputs { config_dir: Some("/abs/omp".into()), ..inputs(&home) });
	assert_eq!(source.base_root(), home.join("abs/omp"));
}

#[test]
fn profiles_follow_omp_profile_then_pi_profile_and_enumerate_valid_names() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let select = |omp: Option<&str>, pi: Option<&str>| {
		V1Source::new(V1Inputs {
			omp_profile: omp.map(Into::into),
			pi_profile: pi.map(Into::into),
			..inputs(&home)
		})
		.active_profile()
		.map(str::to_owned)
	};
	assert_eq!(select(Some("work"), Some("other")).as_deref(), Some("work"));
	assert_eq!(select(None, Some("other")).as_deref(), Some("other"));
	assert_eq!(select(Some(""), Some("other")), None);
	assert_eq!(select(Some("default"), None), None);
	assert_eq!(select(Some("../escape"), None), None, "an invalid profile selects the default");

	let profiles = home.join(".omp/profiles");
	for name in ["work", "alpha", "Bad", "trail."] {
		fs::create_dir_all(profiles.join(name).join("agent")).expect("profile");
	}
	write(&profiles.join("file"), "not a profile");
	let source = V1Source::new(inputs(&home));
	assert_eq!(source.profiles().expect("profiles"), ["alpha", "work"]);
	assert!(source.has_profile("work"));
	assert!(!source.has_profile("missing"));
	assert_eq!(source.layout(Some("work")).config_root(), profiles.join("work"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn xdg_relocation_flattens_the_agent_prefix() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let agent = home.join(".omp/agent");
	let data = root.path().join("xdg-data");
	let state = root.path().join("xdg-state");
	let xdg = V1Inputs {
		xdg_data_home: Some(data.clone()),
		xdg_state_home: Some(state.clone()),
		xdg_cache_home: Some(root.path().join("xdg-cache")),
		..inputs(&home)
	};
	write(&agent.join("config.yml"), "{}");
	write(&agent.join("models.yml"), V1_MODELS_YML);
	write(&agent.join("secret-placeholder.key"), "legacy");
	write(&home.join(".omp/marketplaces.json"), "{}");
	write(&data.join("omp/agent.db"), "db");
	write(&data.join("omp/sessions/a.jsonl"), "{}");
	write(&data.join("omp/plugins/installed_plugins.json"), "{}");
	write(&state.join("omp/memories/mnemopi/mnemopi.db"), "m");

	let layout = V1Source::new(xdg.clone()).layout(None);
	assert_eq!(layout.xdg_root(XdgCategory::Data), Some(data.join("omp").as_path()));
	assert_eq!(layout.xdg_root(XdgCategory::State), Some(state.join("omp").as_path()));
	// `$XDG_CACHE_HOME/omp` does not exist, so the cache stays put.
	assert_eq!(layout.xdg_root(XdgCategory::Cache), None);
	// Agent data flattens: `~/.omp/agent/sessions` → `$XDG_DATA_HOME/omp/sessions`.
	assert_eq!(layout.locate(V1Item::AgentDb), Some(data.join("omp/agent.db")));
	assert_eq!(layout.locate(V1Item::Sessions), Some(data.join("omp/sessions")));
	assert_eq!(layout.locate(V1Item::Plugins), Some(data.join("omp/plugins")));
	assert_eq!(layout.locate(V1Item::MnemopiMemory), Some(state.join("omp/memories/mnemopi")));
	// Configuration never relocates.
	assert_eq!(layout.locate(V1Item::Settings), Some(agent.join("config.yml")));
	assert_eq!(layout.locate(V1Item::Models), Some(agent.join("models.yml")));
	// v1 adopts the unrelocated key and registry until the XDG copy exists.
	assert_eq!(
		layout.locate(V1Item::SecretPlaceholderKey),
		Some(agent.join("secret-placeholder.key"))
	);
	assert_eq!(layout.locate(V1Item::Marketplaces), Some(home.join(".omp/marketplaces.json")));
	write(&state.join("omp/secret-placeholder.key"), "adopted");
	assert_eq!(
		layout.locate(V1Item::SecretPlaceholderKey),
		Some(state.join("omp/secret-placeholder.key"))
	);

	// A named profile relocates only once its own XDG profile dir exists.
	let source = V1Source::new(xdg.clone());
	assert_eq!(source.layout(Some("work")).xdg_root(XdgCategory::Data), None);
	fs::create_dir_all(data.join("omp/profiles/work")).expect("xdg profile");
	assert_eq!(
		source.layout(Some("work")).xdg_root(XdgCategory::Data),
		Some(data.join("omp/profiles/work").as_path())
	);
	// An agent-dir override disables relocation.
	let custom = V1Source::new(V1Inputs { agent_dir: Some(root.path().join("custom")), ..xdg });
	assert_eq!(custom.layout(None).xdg_root(XdgCategory::Data), None);
}

#[test]
fn an_explicit_root_ignores_pi_variables_and_xdg() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let from = root.path().join("backup/.omp");
	let data = root.path().join("xdg-data");
	fs::create_dir_all(data.join("omp")).expect("xdg");
	write(&from.join("agent/models.yml"), V1_MODELS_YML);
	let source = V1Source::new(
		V1Inputs {
			config_dir: Some(".omp-alt".into()),
			agent_dir: Some(root.path().join("custom")),
			omp_profile: Some("work".into()),
			xdg_data_home: Some(data),
			..inputs(&home)
		}
		.with_explicit_root(from.clone()),
	);
	assert_eq!(source.base_root(), from);
	assert_eq!(source.active_profile(), None);
	let layout = source.layout(None);
	assert_eq!(layout.agent_dir(), from.join("agent"));
	assert_eq!(layout.xdg_root(XdgCategory::Data), None);
	assert_eq!(layout.locate(V1Item::Models), Some(from.join("agent/models.yml")));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_v1_xdg_root_shared_with_v2_is_reported() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let v2 = roots(root.path(), None);
	// v1 relocated to `$XDG_DATA_HOME/omp`, which is also v2's data dir.
	fs::create_dir_all(&v2.data_dir).expect("shared data root");
	let source =
		V1Source::new(V1Inputs { xdg_data_home: Some(root.path().join("share")), ..inputs(&home) });
	let pairs = plan(&source, &v2, &ProfileSelection::All).expect("plan");
	assert_eq!(pairs[0].xdg_collisions(), [XdgCollision {
		category: XdgCategory::Data,
		v1:       v2.data_dir.clone(),
		v2:       v2.data_dir.clone(),
	}]);
	let report = run(&pairs, ImportMode::DryRun, CredentialAccess::Offline(&omp_con::Ctx::new()));
	assert_eq!(report.pairs[0].collisions.len(), 1);
	// Reported, never acted on: the dry run left the shared root as it was.
	assert_eq!(fs::read_dir(&v2.data_dir).expect("root").count(), 0);

	// Separate roots do not collide.
	let apart = V1Source::new(V1Inputs {
		xdg_data_home: Some(root.path().join("elsewhere")),
		..inputs(&home)
	});
	fs::create_dir_all(root.path().join("elsewhere/omp")).expect("v1 root");
	let pairs = plan(&apart, &v2, &ProfileSelection::All).expect("plan");
	assert!(pairs[0].xdg_collisions().is_empty());
}

#[test]
fn profiles_pair_per_owner_decision_8() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	for profile in ["work", "alpha"] {
		fs::create_dir_all(home.join(".omp/profiles").join(profile).join("agent")).expect("profile");
	}
	let pairs = |source: &V1Source, active: Option<&str>, selection: ProfileSelection| {
		plan(source, &roots(root.path(), active), &selection)
			.expect("plan")
			.into_iter()
			.map(|pair| {
				(
					pair.source.profile().map(str::to_owned),
					pair.target.profile.as_deref().map(str::to_owned),
				)
			})
			.collect::<Vec<_>>()
	};
	let owned = |profile: &str| Some(profile.to_owned());
	let source = V1Source::new(inputs(&home));
	// By default every v1 profile imports into its v2 namesake, whatever the
	// active v2 profile or v1's own `PI_PROFILE` selection is.
	let everything =
		[(None, None), (owned("alpha"), owned("alpha")), (owned("work"), owned("work"))];
	assert_eq!(ProfileSelection::default(), ProfileSelection::All);
	assert_eq!(pairs(&source, None, ProfileSelection::All), everything);
	assert_eq!(pairs(&source, Some("solo"), ProfileSelection::All), everything);
	let pi = V1Source::new(V1Inputs { pi_profile: Some("alpha".into()), ..inputs(&home) });
	assert_eq!(pairs(&pi, None, ProfileSelection::All), everything);
	// `--profile <p>` restricts the import to one profile, into its namesake.
	assert_eq!(pairs(&source, None, ProfileSelection::Named(Some("work".into()))), [(
		owned("work"),
		owned("work")
	)]);
	assert_eq!(pairs(&source, Some("work"), ProfileSelection::Named(None)), [(None, None)]);
	let target = roots(root.path(), None).target(Some("work"));
	assert_eq!(target.config_dir, root.path().join("o2/profiles/work"));
	assert_eq!(target.data_dir, root.path().join("share/omp/profiles/work"));
}

#[test]
fn a_dry_run_writes_nothing() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(&home.join(".omp/agent/models.yml"), V1_MODELS_YML);
	write(&home.join(".omp/agent/sessions/s.jsonl"), "{}");
	let v2 = roots(root.path(), None);
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");
	let before = snapshot(root.path());

	let report = run(&pairs, ImportMode::DryRun, CredentialAccess::Offline(&omp_con::Ctx::new()));

	assert_eq!(snapshot(root.path()), before, "a dry run must not write anywhere");
	assert!(report.dry_run);
	assert_eq!(outcomes(&report), [
		(ImportStep::Models, None, OutcomeKind::WouldImport),
		(ImportStep::ModelsKeys, Some("easycliproxy"), OutcomeKind::WouldImport),
		(ImportStep::ModelsKeys, Some("envkey"), OutcomeKind::NeedsAttention),
	]);
	let inventory = &report.pairs[0].inventory;
	assert_eq!(inventory[0].0, V1Item::Models);
	assert_eq!(inventory[0].2, [ImportStep::Models, ImportStep::ModelsKeys]);
	assert_eq!(inventory[1].0, V1Item::Sessions);
	assert!(inventory[1].2.is_empty(), "no session step is registered yet");
}

#[test]
fn an_import_copies_once_and_leaves_the_v1_tree_byte_identical() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let omp = home.join(".omp");
	write(&omp.join("agent/models.yml"), V1_MODELS_YML);
	write(&omp.join("agent/config.yml"), "theme: dark\n");
	write(&omp.join("profiles/work/agent/models.yml"), "providers: {}\n");
	let v1_before = snapshot(&omp);
	let v2 = roots(root.path(), None);
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");
	let (control, store) = control(root.path());
	// The live store belongs to the default profile, as in the first-run hook.
	let live = CredentialAccess::Live { data_dir: &v2.data_dir, control: &control };

	let report = run(&pairs, ImportMode::Apply, live);

	assert_eq!(outcomes(&report), [
		(ImportStep::Models, None, OutcomeKind::Imported),
		(ImportStep::ModelsKeys, Some("easycliproxy"), OutcomeKind::Imported),
		(ImportStep::ModelsKeys, Some("envkey"), OutcomeKind::NeedsAttention),
		// The `work` profile's config imports into its v2 namesake; its
		// credentials wait for its own live store, unmarked.
		(ImportStep::Models, None, OutcomeKind::Imported),
		(ImportStep::ModelsKeys, None, OutcomeKind::Skipped),
	]);
	let work = v2.config_dir.join("profiles/work");
	assert!(work.join("models.toml").is_file());
	assert!(ImportStep::Models.marker(&work).is_set());
	assert!(!ImportStep::ModelsKeys.marker(&work).is_set());
	assert!(matches!(
		report.pairs[1].entries[1].outcome,
		ImportOutcome::Skipped(SkipReason::WaitsForProfile)
	));
	let config = v2.config_dir.as_path();
	let native = fs::read_to_string(config.join("models.toml")).expect("models.toml");
	assert!(native.contains("[providers.easycliproxy"), "{native}");
	assert!(!native.contains("sk-literal-key"), "{native}");
	assert_eq!(
		fs::read_to_string(config.join(".models-migration-v1")).expect("marker"),
		"revision = 1\nsource = \"legacy-yaml\"\n"
	);
	assert!(config.join(".models-keys-migration-v1").is_file());
	assert_eq!(
		store
			.list_metadata()
			.expect("metadata")
			.iter()
			.map(|row| row.account_id.as_str().to_owned())
			.collect::<Vec<_>>(),
		["easycliproxy:models-yml"]
	);
	assert_eq!(
		control
			.accounts(Some(ProviderId::from_ref("easycliproxy")))
			.len(),
		1
	);
	// Copy only: every v1 file and directory is exactly as it was.
	assert_eq!(snapshot(&omp), v1_before);

	// A second run is a no-op through the markers, even after v1 changes.
	write(&omp.join("agent/models.yml"), "providers:\n  late:\n    apiKey: sk-late\n");
	let v2_before = snapshot(config);
	let again = run(&pairs[..1], ImportMode::Apply, live);
	assert_eq!(outcomes(&again), [
		(ImportStep::Models, None, OutcomeKind::Skipped),
		(ImportStep::ModelsKeys, None, OutcomeKind::Skipped),
	]);
	assert!(
		again
			.entries()
			.all(|entry| matches!(entry.outcome, ImportOutcome::Skipped(SkipReason::MarkerPresent)))
	);
	assert_eq!(snapshot(config), v2_before);
	assert_eq!(store.list_metadata().expect("metadata").len(), 1);
}

#[test]
fn a_failed_step_is_reported_and_retried() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(
		&home.join(".omp/agent/models.yml"),
		"providers:\n  demo:\n    models:\n      - name: x\n",
	);
	let v2 = roots(root.path(), None);
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &ProfileSelection::All).expect("plan");
	let (control, _) = control(root.path());
	let report = run(&pairs, ImportMode::Apply, CredentialAccess::Live {
		data_dir: &v2.data_dir,
		control:  &control,
	});
	let models = report.entries().next().expect("models entry");
	assert!(matches!(
		models.outcome,
		ImportOutcome::NeedsAttention(Attention::Failed(ImportError::Models(_)))
	));
	assert!(!ImportStep::Models.marker(&v2.config_dir).is_set(), "a failure stays retryable");
}

#[test]
fn the_models_import_honours_a_v1_profile() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	// Only the `work` profile has model config; the default agent dir is empty.
	write(&home.join(".omp/profiles/work/agent/models.yml"), V1_MODELS_YML);
	let v2 = roots(root.path(), Some("work"));
	let selection = ProfileSelection::Named(v2.active_profile.clone());
	let pairs = plan(&V1Source::new(inputs(&home)), &v2, &selection).expect("plan");
	let location = ModelsConfigLocation::for_pair(&pairs[0]);
	assert_eq!(location.config_dir, root.path().join("o2/profiles/work"));

	let loaded = load_or_import_legacy(&location)
		.expect("import")
		.expect("the work profile's models.yml is imported");
	assert!(loaded.config.providers.contains_key("easycliproxy"));
	assert!(root.path().join("o2/profiles/work/models.toml").is_file());
	assert!(!root.path().join("o2/models.toml").exists(), "the default profile is untouched");
}

#[test]
fn the_models_import_honours_pi_coding_agent_dir() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let custom = root.path().join("custom-agent");
	write(&custom.join("models.yaml"), V1_MODELS_YML);
	let source = V1Source::new(V1Inputs { agent_dir: Some(custom.clone()), ..inputs(&home) });
	let pairs = plan(&source, &roots(root.path(), None), &ProfileSelection::All).expect("plan");
	let location = ModelsConfigLocation::for_pair(&pairs[0]);
	assert_eq!(location.legacy_path(), Some(custom.join("models.yaml")));
	assert!(load_or_import_legacy(&location).expect("import").is_some());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_models_import_under_xdg_reads_the_agent_models_yml() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let data = root.path().join("xdg-data");
	// v1 never relocated `models.yml`: a copy under the XDG data root is not
	// what v1 read, and must not be imported.
	write(&data.join("omp/models.yml"), "providers:\n  decoy: {}\n");
	write(&data.join("omp/agent.db"), "db");
	write(&home.join(".omp/agent/models.yml"), V1_MODELS_YML);
	let source = V1Source::new(V1Inputs { xdg_data_home: Some(data.clone()), ..inputs(&home) });
	let pairs = plan(&source, &roots(root.path(), None), &ProfileSelection::All).expect("plan");
	assert_eq!(pairs[0].source.locate(V1Item::AgentDb), Some(data.join("omp/agent.db")));
	let loaded = load_or_import_legacy(&ModelsConfigLocation::for_pair(&pairs[0]))
		.expect("import")
		.expect("config");
	assert!(loaded.config.providers.contains_key("easycliproxy"));
	assert!(!loaded.config.providers.contains_key("decoy"));
}
