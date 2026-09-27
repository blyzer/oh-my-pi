//! The `keybindings` step over temporary homes, v1 trees, and v2 roots.

use omp_con::Source;
use omp_core::Str;

use super::*;
use crate::keybindings::{DEFAULT_BINDS, DEFAULT_BINDS_NAME};

fn import(home: &Path, v2: &V2Roots, mode: ImportMode) -> ImportReport {
	let pairs = plan(&V1Source::new(inputs(home)), v2, &ProfileSelection::All).expect("plan");
	run(&pairs, mode, CredentialAccess::Offline(&omp_con::Ctx::new()))
}

/// The keybindings entries of one pair: subject, outcome, and file read.
fn entries(report: &ImportReport, pair: usize) -> Vec<(Option<String>, String, Option<PathBuf>)> {
	report.pairs[pair]
		.entries
		.iter()
		.filter(|entry| entry.step == ImportStep::Keybindings)
		.map(|entry| {
			let outcome = match &entry.outcome {
				ImportOutcome::Skipped(reason) => format!("skipped: {reason}"),
				ImportOutcome::NotMigratable(reason) => format!("not-migratable: {reason}"),
				ImportOutcome::NeedsAttention(attention) => format!("needs-attention: {attention}"),
				outcome => outcome.kind().to_string(),
			};
			(entry.subject.as_deref().map(str::to_owned), outcome, entry.path.clone())
		})
		.collect()
}

fn subjects(report: &ImportReport, pair: usize) -> Vec<(String, String)> {
	entries(report, pair)
		.into_iter()
		.map(|(subject, outcome, _)| (subject.unwrap_or_default(), outcome))
		.collect()
}

fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
	pairs
		.iter()
		.map(|(subject, outcome)| ((*subject).to_owned(), (*outcome).to_owned()))
		.collect()
}

/// The bind table v2 runs with: the default bind cfg, then `config.cfg`,
/// executed strictly so every imported line must parse and run.
fn effective(config: &Path) -> omp_con::Ctx {
	let ctx = omp_con::Ctx::new();
	ctx.exec(DEFAULT_BINDS, Source::Config(Str::new_static(DEFAULT_BINDS_NAME)))
		.expect("default binds");
	if let Ok(text) = fs::read_to_string(config) {
		ctx.exec(&text, Source::Config(Str::new_static("config.cfg")))
			.expect("the imported config.cfg replays strictly");
	}
	ctx
}

fn bound(ctx: &omp_con::Ctx, chord: &str) -> Option<String> {
	ctx.bound(chord).map(|script| script.as_str().to_owned())
}

#[test]
fn single_and_multiple_chords_import_with_normalized_modifiers() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let file = home.join(".omp/agent/keybindings.yml");
	write(
		&file,
		concat!(
			"app.model.select: Shift+Ctrl+L\n",
			"app.retry: [f6, ALT+R, alt+r]\n",
			"selectModelTemporary: alt+ctrl+p\n",
			"app.session.new: ctrl+n\n",
			"app.history.search: ctrl+f\n",
			"tui.editor.pageUp: pageUp\n",
		),
	);
	let v2 = roots(root.path(), None);

	let report = import(&home, &v2, ImportMode::Apply);

	let imported = "imported";
	assert_eq!(
		subjects(&report, 0),
		owned(&[
			("app.model.select: ctrl+shift+l", imported),
			("app.retry: f6", imported),
			("app.retry: alt+r", imported),
			("app.model.selectTemporary: ctrl+alt+p", imported),
			("app.session.new: ctrl+n", imported),
			("app.history.search: ctrl+f", imported),
			("tui.editor.pageUp: pageup", imported),
		])
	);
	assert!(
		entries(&report, 0)
			.iter()
			.all(|(_, _, path)| path.as_deref() == Some(&*file))
	);
	let config = v2.config_dir.join("config.cfg");
	assert_eq!(
		fs::read_to_string(&config).expect("config.cfg"),
		format!(
			concat!(
				"// Imported from omp v1 keybindings: {}\n",
				"unbind alt+m\n",
				"unbind alt+p\n",
				"unbind f5\n",
				"bind ctrl+alt+p \"cl_model_select session\"\n",
				"bind ctrl+f \"cl_history_search; ed_right\"\n",
				"bind ctrl+n new\n",
				"bind ctrl+r panel_rename\n",
				"bind ctrl+shift+l cl_model_select\n",
				"bind f6 cl_retry\n",
			),
			file.display()
		)
	);
	let ctx = effective(&config);
	// A v1 remap replaces the action's default chords, as v1 did.
	assert_eq!(bound(&ctx, "alt+m"), None);
	assert_eq!(bound(&ctx, "shift+ctrl+l").as_deref(), Some("cl_model_select"));
	assert_eq!(bound(&ctx, "f5"), None);
	assert_eq!(bound(&ctx, "f6").as_deref(), Some("cl_retry"));
	assert_eq!(bound(&ctx, "alt+r").as_deref(), Some("cl_retry"));
	// Contextual commands sharing a chord stay: the panel keeps Ctrl+R, and
	// the base editor still falls back on Ctrl+F.
	assert_eq!(bound(&ctx, "ctrl+r").as_deref(), Some("panel_rename"));
	assert_eq!(bound(&ctx, "ctrl+f").as_deref(), Some("cl_history_search; ed_right"));
	// A remap of one of two actions sharing a command keeps the other's
	// chords (`tui.select.pageUp` also runs `ed_page_up`).
	assert_eq!(bound(&ctx, "pageup").as_deref(), Some("ed_page_up"));
}

#[test]
fn unknown_actions_and_invalid_values_are_reported_and_commented() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(
		&home.join(".omp/agent/keybindings.yml"),
		concat!(
			"app.teleport: [ctrl+x, alt+x]\n",
			"app.exit: 5\n",
			"app.clear: \"ctrl x\"\n",
			"app.suspend: ~\n",
		),
	);
	let v2 = roots(root.path(), None);

	let report = import(&home, &v2, ImportMode::Apply);

	assert_eq!(
		subjects(&report, 0),
		owned(&[
			("app.teleport", "not-migratable: v2 has no equivalent"),
			(
				"app.exit",
				"needs-attention: not a key chord v2 understands; rebind it with `bind <chord> \
				 <command>`"
			),
			(
				"app.clear: ctrl x",
				"needs-attention: not a key chord v2 understands; rebind it with `bind <chord> \
				 <command>`"
			),
			("app.suspend", "nothing-to-import"),
		])
	);
	let config = v2.config_dir.join("config.cfg");
	let text = fs::read_to_string(&config).expect("config.cfg");
	assert!(
		text.contains("// not imported: v1 `app.teleport` (ctrl+x, alt+x) has no v2 command\n"),
		"{text}"
	);
	assert!(
		text.contains("// not imported: v1 `app.exit` is not a chord or chord list\n"),
		"{text}"
	);
	assert!(
		text.contains("// not imported: v1 `app.clear` chord `ctrl x` is not a key chord\n"),
		"{text}"
	);
	assert!(!text.contains("\nbind") && !text.contains("\nunbind"), "{text}");
	// Nothing importable replaced a default.
	let ctx = effective(&config);
	assert_eq!(bound(&ctx, "ctrl+c").as_deref(), Some("cl_clear; cl_interrupt; ed_copy"));
	assert_eq!(bound(&ctx, "ctrl+z").as_deref(), Some("cl_suspend"));
}

#[test]
fn an_existing_v2_bind_wins_and_the_import_appends() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	write(
		&home.join(".omp/agent/keybindings.yml"),
		concat!(
			"app.model.select: [ctrl+l, alt+x]\n",
			"app.model.selectTemporary: alt+p\n",
			"app.thinking.toggle: Ctrl+T\n",
		),
	);
	let v2 = roots(root.path(), None);
	let config = v2.config_dir.join("config.cfg");
	let existing = concat!(
		"// mine\n",
		"bind ctrl+l cl_model_cycle\n",
		"unbind alt+p\n",
		"bind ctrl+t \"toggle cl_showthinking\"",
	);
	write(&config, existing);

	let report = import(&home, &v2, ImportMode::Apply);

	let kept = "skipped: v2 config.cfg already binds this chord; kept the existing bind";
	assert_eq!(
		subjects(&report, 0),
		owned(&[
			("app.model.select: ctrl+l", kept),
			("app.model.select: alt+x", "imported"),
			("app.model.selectTemporary: alt+p", kept),
			("app.thinking.toggle: ctrl+t", kept),
		])
	);
	let text = fs::read_to_string(&config).expect("config.cfg");
	assert!(text.starts_with(existing), "the existing text is never rewritten:\n{text}");
	assert!(text.contains("\n\n// Imported from omp v1 keybindings: "), "{text}");
	assert!(text.contains("// kept the existing bind for ctrl+l over v1 `app.model.select`\n"));
	let ctx = effective(&config);
	assert_eq!(bound(&ctx, "ctrl+l").as_deref(), Some("cl_model_cycle"));
	assert_eq!(bound(&ctx, "alt+x").as_deref(), Some("cl_model_select"));
	assert_eq!(bound(&ctx, "alt+m"), None, "an installed remap still replaces the default");
	assert_eq!(bound(&ctx, "alt+p"), None);
	assert_eq!(bound(&ctx, "ctrl+t").as_deref(), Some("toggle cl_showthinking"));
}

#[test]
fn named_profiles_inherit_the_default_profiles_bindings() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let omp = home.join(".omp");
	let default_file = omp.join("agent/keybindings.yml");
	write(&default_file, "app.model.select: ctrl+l\napp.session.new: ctrl+n\n");
	// `work` has no file of its own; `alpha` overrides one action and adds
	// another; `beta` has none, and its v2 profile already has binds.
	fs::create_dir_all(omp.join("profiles/work/agent")).expect("work");
	let alpha_file = omp.join("profiles/alpha/agent/keybindings.yml");
	write(&alpha_file, "app.model.select: ctrl+k\napp.retry: f6\n");
	fs::create_dir_all(omp.join("profiles/beta/agent")).expect("beta");
	let v2 = roots(root.path(), None);
	let beta_config = v2.config_dir.join("profiles/beta/config.cfg");
	write(&beta_config, "bind f9 cl_retry\n");

	let report = import(&home, &v2, ImportMode::Apply);

	let profiles = report
		.pairs
		.iter()
		.map(|pair| pair.target_profile.as_deref().map(str::to_owned))
		.collect::<Vec<_>>();
	assert_eq!(profiles, [None, Some("alpha".into()), Some("beta".into()), Some("work".into())]);
	// alpha: v1 merges `{ ...default, ...alpha }` action by action.
	assert_eq!(entries(&report, 1), [
		(Some("app.model.select: ctrl+k".into()), "imported".into(), Some(alpha_file.clone())),
		(Some("app.session.new: ctrl+n".into()), "imported".into(), Some(default_file.clone())),
		(Some("app.retry: f6".into()), "imported".into(), Some(alpha_file)),
	]);
	let alpha = effective(&v2.config_dir.join("profiles/alpha/config.cfg"));
	// The app action goes ahead of the base editor on a shared chord.
	assert_eq!(bound(&alpha, "ctrl+k").as_deref(), Some("cl_model_select; ed_delete_to_end"));
	assert_eq!(bound(&alpha, "ctrl+l").as_deref(), Some("live"), "alpha's own value wins");
	assert_eq!(bound(&alpha, "ctrl+n").as_deref(), Some("new"));
	// beta: the v2 profile's own binds win over what v1 would inherit.
	let target_exists = "skipped: v2 already has its own copy";
	assert_eq!(
		subjects(&report, 2),
		owned(&[("app.model.select", target_exists), ("app.session.new", target_exists)])
	);
	assert_eq!(fs::read_to_string(&beta_config).expect("beta"), "bind f9 cl_retry\n");
	assert!(
		ImportStep::Keybindings
			.marker(&v2.config_dir.join("profiles/beta"))
			.is_set()
	);
	// work: the default profile's bindings, read from the default file.
	assert_eq!(entries(&report, 3), [
		(Some("app.model.select: ctrl+l".into()), "imported".into(), Some(default_file.clone())),
		(Some("app.session.new: ctrl+n".into()), "imported".into(), Some(default_file.clone())),
	]);
	let work_config = v2.config_dir.join("profiles/work/config.cfg");
	let text = fs::read_to_string(&work_config).expect("work");
	assert!(
		text.starts_with(&format!(
			"// Imported from omp v1 keybindings\n// inherited from the v1 default profile: {}\n",
			default_file.display()
		)),
		"{text}"
	);
	let work = effective(&work_config);
	assert_eq!(bound(&work, "ctrl+l").as_deref(), Some("cl_model_select; live"));
	assert_eq!(bound(&work, "alt+m"), None);
}

#[test]
fn a_legacy_json_file_imports_when_no_yml_exists() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let agent = home.join(".omp/agent");
	let json = agent.join("keybindings.json");
	write(
		&json,
		concat!(
			"{\n",
			"  // v1 wrote JSONC before it moved to yml\n",
			"  \"selectModel\": \"ctrl+l\",\n",
			"  \"app.retry\": [\"f6\",],\n",
			"}\n",
		),
	);
	let v2 = roots(root.path(), None);

	let report = import(&home, &v2, ImportMode::DryRun);
	assert_eq!(entries(&report, 0), [
		(Some("app.model.select: ctrl+l".into()), "would-import".into(), Some(json.clone())),
		(Some("app.retry: f6".into()), "would-import".into(), Some(json.clone())),
	]);

	// v1 reads the yml once it exists; the json is only a fallback.
	let yml = agent.join("keybindings.yml");
	write(&yml, "app.retry: f7\n");
	let report = import(&home, &v2, ImportMode::Apply);
	assert_eq!(entries(&report, 0), [(Some("app.retry: f7".into()), "imported".into(), Some(yml))]);
	assert!(
		fs::read_to_string(&json)
			.expect("json")
			.contains("selectModel"),
		"json untouched"
	);
}

#[test]
fn the_keybindings_import_keeps_the_framework_invariants() {
	let root = tempfile::tempdir().expect("scratch");
	let home = root.path().join("home");
	let omp = home.join(".omp");
	write(&omp.join("agent/keybindings.yml"), "app.model.select: ctrl+l\n");
	write(&omp.join("profiles/work/agent/keybindings.yaml"), "app.retry: [f6]\n");
	let v2 = roots(root.path(), None);

	// A dry run writes nothing anywhere.
	let before = snapshot(root.path());
	let dry = import(&home, &v2, ImportMode::DryRun);
	assert_eq!(snapshot(root.path()), before, "a dry run must not write anywhere");
	assert_eq!(subjects(&dry, 0), owned(&[("app.model.select: ctrl+l", "would-import")]));
	assert_eq!(
		subjects(&dry, 1),
		owned(&[("app.model.select: ctrl+l", "would-import"), ("app.retry: f6", "would-import")])
	);

	// An apply copies into each profile and leaves v1 byte-identical.
	let v1_before = snapshot(&omp);
	let applied = import(&home, &v2, ImportMode::Apply);
	assert_eq!(snapshot(&omp), v1_before, "the v1 tree is never written");
	assert_eq!(subjects(&applied, 1).len(), 2);
	for config_dir in [v2.config_dir.clone(), v2.config_dir.join("profiles/work")] {
		assert!(config_dir.join("config.cfg").is_file());
		assert!(ImportStep::Keybindings.marker(&config_dir).is_set());
	}
	let work = effective(&v2.config_dir.join("profiles/work/config.cfg"));
	assert_eq!(bound(&work, "f6").as_deref(), Some("cl_retry"));
	assert_eq!(bound(&work, "f5"), None);

	// A second run is a no-op through the marker, even after v1 changes.
	write(&omp.join("agent/keybindings.yml"), "app.model.select: ctrl+k\n");
	let v2_before = snapshot(&v2.config_dir);
	let again = import(&home, &v2, ImportMode::Apply);
	assert_eq!(snapshot(&v2.config_dir), v2_before);
	let markers = again
		.entries()
		.filter(|entry| entry.step == ImportStep::Keybindings)
		.map(|entry| matches!(entry.outcome, ImportOutcome::Skipped(SkipReason::MarkerPresent)))
		.collect::<Vec<_>>();
	assert_eq!(markers, [true, true]);
}
