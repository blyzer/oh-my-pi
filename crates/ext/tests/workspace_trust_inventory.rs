//! The gated-input inventory and its digest: what feeds it, how it is
//! encoded, and every way it fails closed.

use std::{
	env, fs,
	path::{Component, Path, PathBuf},
	time::SystemTime,
};

use omp_core::{Hash32, Str, project_file::Refusal};
use omp_ext::{
	claude_plugin::{
		InstallScope, InstalledPluginEntry, InstalledPluginsRegistry, REGISTRY_FILE,
		plugin_cache_dir, project_plugins_dir, user_plugins_dir,
	},
	workspace_trust::{
		InputsDigest, WorkspaceTrustError,
		inventory::{
			AGENT_PLUGIN_DIRS, BudgetLimit, CLAUDE_SETTINGS_FILES, EXTENSIONS_DIR, INVENTORY_DOMAIN,
			InputKind, InputsInventory, InventoryBudget, PROJECT_PLUGINS_DIR, ROOT_MCP_FILE,
			WORKFLOWS_DIR, gated_files, inventory,
		},
	},
};

/// A canonical scratch tree: a repository (its own containment root), a data
/// directory, and a directory outside both.
struct Fixture {
	_temp:   tempfile::TempDir,
	repo:    PathBuf,
	data:    PathBuf,
	outside: PathBuf,
}

fn fixture() -> Fixture {
	let temp = tempfile::tempdir().expect("scratch");
	let root = fs::canonicalize(temp.path()).expect("canonical scratch");
	let repo = root.join("repo");
	let data = root.join("data");
	let outside = root.join("outside");
	for dir in [repo.join(".git"), data.clone(), outside.clone()] {
		fs::create_dir_all(dir).expect("fixture directory");
	}
	Fixture { _temp: temp, repo, data, outside }
}

fn write(path: &Path, body: impl AsRef<[u8]>) {
	fs::create_dir_all(path.parent().expect("parent")).expect("parent directory");
	fs::write(path, body).expect("fixture file");
}

impl Fixture {
	fn take_with(&self, budget: &InventoryBudget) -> Result<InputsInventory, WorkspaceTrustError> {
		inventory(&self.repo, &self.data, budget)
	}

	fn take(&self) -> InputsInventory {
		self
			.take_with(&InventoryBudget::default())
			.expect("inventory")
	}

	fn digest(&self) -> InputsDigest {
		*self.take().digest()
	}

	fn error(&self) -> WorkspaceTrustError {
		self
			.take_with(&InventoryBudget::default())
			.expect_err("inventory must fail closed")
	}

	fn write(&self, relative: &str, body: impl AsRef<[u8]>) {
		write(&self.repo.join(relative), body);
	}

	/// Records `installs` (`id`, install path, enabled) in the project plugin
	/// registry.
	fn register(&self, installs: &[(&str, &Path, bool)]) {
		let mut registry = InstalledPluginsRegistry::default();
		for (id, path, enabled) in installs {
			registry
				.plugins
				.insert(Str::new(id), vec![InstalledPluginEntry {
					scope:          InstallScope::Project,
					install_path:   path.to_path_buf(),
					version:        Str::new_static("1.0.0"),
					installed_at:   Str::new_static("2026-10-07T00:00:00Z"),
					last_updated:   Str::new_static("2026-10-07T00:00:00Z"),
					git_commit_sha: None,
					enabled:        *enabled,
				}]);
		}
		write(
			&project_plugins_dir(&self.repo).join(REGISTRY_FILE),
			serde_json::to_vec(&registry).expect("registry"),
		);
	}
}

/// `relative`'s components joined with `/`.
fn key(relative: &Path) -> Vec<u8> {
	let mut key = Vec::new();
	for component in relative.components() {
		if !key.is_empty() {
			key.push(b'/');
		}
		key.extend_from_slice(component.as_os_str().as_encoded_bytes());
	}
	key
}

/// The `(path, kind)` of every entry that exists.
fn present(inventory: &InputsInventory) -> Vec<(String, InputKind)> {
	inventory
		.present()
		.map(|entry| (String::from_utf8(entry.path().to_vec()).expect("UTF-8 path"), entry.kind()))
		.collect()
}

/// The SHA-256 the digest specification gives for `entries`, sorted by path.
fn encode(entries: &mut [(Vec<u8>, u8, [u8; 32])]) -> InputsDigest {
	entries.sort();
	let mut hasher = Hash32::hasher();
	hasher
		.update(INVENTORY_DOMAIN)
		.update((entries.len() as u64).to_le_bytes());
	for (path, tag, hash) in entries.iter() {
		hasher
			.update((path.len() as u64).to_le_bytes())
			.update(path)
			.update([*tag])
			.update(hash);
	}
	InputsDigest::new(hasher.finalize())
}

/// Every fixed entry of an empty workspace: absent, with a zero hash.
fn absent_fixed_entries() -> Vec<(Vec<u8>, u8, [u8; 32])> {
	gated_files()
		.map(|gated| gated.relative())
		.chain(CLAUDE_SETTINGS_FILES.iter().map(PathBuf::from))
		.chain([Path::new(PROJECT_PLUGINS_DIR).join(REGISTRY_FILE)])
		.map(|path| (key(&path), 0, [0; 32]))
		.collect()
}

#[test]
fn kind_tags_are_pinned() {
	let tags = [
		(InputKind::Absent, 0),
		(InputKind::Bytes, 1),
		(InputKind::EnabledPlugins, 2),
		(InputKind::TreeFile, 3),
		(InputKind::TreeExecutable, 4),
	];
	for (kind, tag) in tags {
		assert_eq!(kind.tag(), tag, "{kind:?}");
	}
	assert_eq!(INVENTORY_DOMAIN, b"omp.workspace-trust.inputs.v1\0");
}

#[test]
fn an_empty_workspace_digest_is_pinned_and_follows_the_encoding() {
	let fixture = fixture();
	let inventory = fixture.take();
	assert_eq!(inventory.entries().len(), 47, "44 fixed files, 2 settings files, 1 registry");
	assert!(
		inventory
			.entries()
			.all(|entry| entry.kind() == InputKind::Absent)
	);
	assert_eq!(inventory.present().count(), 0);
	assert_eq!(*inventory.digest(), encode(&mut absent_fixed_entries()));
	assert_eq!(
		inventory.digest().to_string(),
		"sha256:53fa1db732ccfef98b8bb94c5d9f34deb92818630220d8a3691923216b6550fb",
		"the empty-workspace digest changes only with the inventory list or its encoding"
	);
}

#[cfg(unix)]
#[test]
fn a_populated_workspace_digest_is_pinned_and_follows_the_encoding() {
	use std::os::unix::fs::PermissionsExt as _;

	let fixture = fixture();
	fixture.write(ROOT_MCP_FILE, "{}");
	fixture.write(".omp/hosts.toml", "");
	fixture.write(
		".claude/settings.json",
		r#"{"enabledPlugins":{"b@m":false,"a@m":true,"c@m":"yes"},"permissions":{}}"#,
	);
	fixture.write(".omp/extensions/x/run.sh", "echo\n");
	fixture.write(".omp/extensions/x/lib.py", "x = 1\n");
	fixture.write(".omp/workflows/build.toml", "[[phase]]\n");
	for (file, mode) in [("run.sh", 0o755), ("lib.py", 0o644)] {
		let path = fixture.repo.join(".omp/extensions/x").join(file);
		fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("mode");
	}
	let inventory = fixture.take();

	let mut projection = Hash32::hasher();
	projection.update(2_u64.to_le_bytes());
	for (id, enabled) in [("a@m", 1), ("b@m", 0)] {
		projection
			.update((id.len() as u64).to_le_bytes())
			.update(id)
			.update([enabled]);
	}
	let present: [(&str, u8, Hash32); 6] = [
		(ROOT_MCP_FILE, 1, Hash32::sum("{}")),
		(".omp/hosts.toml", 1, Hash32::sum("")),
		(".claude/settings.json", 2, projection.finalize()),
		(".omp/extensions/x/run.sh", 4, Hash32::sum("echo\n")),
		(".omp/extensions/x/lib.py", 3, Hash32::sum("x = 1\n")),
		(".omp/workflows/build.toml", 1, Hash32::sum("[[phase]]\n")),
	];
	let mut expected = absent_fixed_entries();
	expected.retain(|(path, ..)| !present.iter().any(|(name, ..)| path == name.as_bytes()));
	expected.extend(
		present
			.iter()
			.map(|(path, tag, hash)| (path.as_bytes().to_vec(), *tag, hash.into_bytes())),
	);
	assert_eq!(*inventory.digest(), encode(&mut expected));
	assert_eq!(
		inventory.digest().to_string(),
		"sha256:a962792ca6d0198042c8c98d97d9674607f928ea4a43aca944b52d8c8544a74c",
		"a populated workspace's digest changes only with its inputs or the encoding"
	);
	let paths = inventory
		.entries()
		.map(|entry| entry.path().to_vec())
		.collect::<Vec<_>>();
	assert!(paths.is_sorted(), "entries are in path-byte order");
}

#[test]
fn the_digest_ignores_creation_order_and_times() {
	let first = fixture();
	let second = fixture();
	let files = [
		(".omp/extensions/a/omp.toml", "a"),
		(".omp/extensions/b/main.py", "b"),
		(".agents/plugins/p/plugin.json", "{}"),
		(".omp/mcp.json", "{}"),
		(".vscode/mcp.json", "{}"),
	];
	for (path, body) in files {
		first.write(path, body);
	}
	for (path, body) in files.iter().rev() {
		second.write(path, body);
		let file = fs::File::options()
			.write(true)
			.open(second.repo.join(path))
			.expect("open");
		file.set_modified(SystemTime::UNIX_EPOCH).expect("mtime");
	}
	assert_eq!(first.digest(), second.digest());
	assert_eq!(first.digest(), first.digest(), "recomputing gives the same digest");
	fs::rename(
		first.repo.join(".omp/extensions/b/main.py"),
		first.repo.join(".omp/extensions/b/other.py"),
	)
	.expect("rename");
	assert_ne!(first.digest(), second.digest(), "a renamed tree file changes the digest");
}

#[test]
fn absent_empty_and_present_inputs_differ() {
	let files = fixture();
	let absent = files.digest();
	files.write(ROOT_MCP_FILE, "");
	let empty = files.digest();
	files.write(ROOT_MCP_FILE, "{}");
	let object = files.digest();
	assert_ne!(absent, empty);
	assert_ne!(empty, object);
	assert_ne!(absent, object);

	let trees = fixture();
	let without = trees.digest();
	fs::create_dir_all(trees.repo.join(EXTENSIONS_DIR)).expect("empty tree");
	fs::create_dir_all(trees.repo.join(WORKFLOWS_DIR)).expect("empty workflows");
	assert_eq!(trees.digest(), without, "an empty tree is an absent one");
}

#[test]
fn every_gated_input_feeds_the_digest_and_excluded_files_do_not() {
	let fixture = fixture();
	let mut digest = fixture.digest();
	let mut gated = gated_files()
		.map(|gated| gated.relative())
		.collect::<Vec<_>>();
	gated.extend(
		AGENT_PLUGIN_DIRS
			.iter()
			.map(|dir| Path::new(dir).join("pkg/file.py")),
	);
	gated.extend([
		Path::new(WORKFLOWS_DIR).join("deploy.toml"),
		Path::new(PROJECT_PLUGINS_DIR).join(REGISTRY_FILE),
		PathBuf::from(CLAUDE_SETTINGS_FILES[0]),
		PathBuf::from(CLAUDE_SETTINGS_FILES[1]),
	]);
	for path in &gated {
		fixture.write(path.to_str().expect("UTF-8"), r#"{"enabledPlugins":{"a@m":true}}"#);
		let next = fixture.digest();
		assert_ne!(next, digest, "{} feeds the digest", path.display());
		digest = next;
	}
	for excluded in [
		"AGENTS.md",
		"CLAUDE.md",
		".omp/rules/style.md",
		".omp/skills/s/SKILL.md",
		".omp/config.cfg",
		".omp/omp.lock",
		".omp/installed.toml",
		".omp/prompts/review.md",
		".omp/plugins/node_modules/x/index.js",
		".omp/workflows/notes.md",
		".omp/extensions/.DS_Store",
		".omp/extensions/pkg/Thumbs.db",
		".omp/extensions/pkg/.git/HEAD",
		"node_modules/.bin/typescript-language-server",
		"src/main.rs",
	] {
		fixture.write(excluded, "x");
		assert_eq!(fixture.digest(), digest, "{excluded} stays out of the digest");
	}
}

#[test]
fn only_the_enabled_plugins_projection_of_claude_settings_counts() {
	let fixture = fixture();
	let local = CLAUDE_SETTINGS_FILES[1];
	fixture.write(local, r#"{"enabledPlugins":{"p@m":true},"permissions":{"allow":["Bash(ls)"]}}"#);
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [(local.to_owned(), InputKind::EnabledPlugins)]);
	let enabled = *inventory.digest();
	fixture.write(
		local,
		r#"{"permissions":{"allow":["Bash(ls)","Bash(cat)"]},"enabledPlugins":{"p@m":true}}"#,
	);
	assert_eq!(fixture.digest(), enabled, "a permission edit keeps the digest");
	fixture.write(local, r#"{"enabledPlugins":{"p@m":true,"q@m":"later"}}"#);
	assert_eq!(fixture.digest(), enabled, "a non-boolean value is not read");
	fixture.write(local, r#"{"enabledPlugins":{"p@m":false}}"#);
	let disabled = fixture.digest();
	assert_ne!(disabled, enabled, "flipping enabledPlugins changes the digest");
	fixture.write(local, r#"{"theme":"dark"}"#);
	let none = fixture.digest();
	assert_ne!(none, disabled);

	fixture.write(local, "{not json");
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [(local.to_owned(), InputKind::Bytes)]);
	let broken = *inventory.digest();
	fixture.write(local, "{still not json");
	assert_ne!(fixture.digest(), broken, "an unparsed file is hashed whole");
}

#[cfg(unix)]
#[test]
fn an_execute_bit_change_changes_the_digest() {
	use std::os::unix::fs::PermissionsExt as _;

	let fixture = fixture();
	let script = fixture.repo.join(".agent/plugins/p/bin/server");
	write(&script, "#!/bin/sh\n");
	fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).expect("mode");
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [(
		".agent/plugins/p/bin/server".to_owned(),
		InputKind::TreeFile
	)]);
	let plain = *inventory.digest();
	fs::set_permissions(&script, fs::Permissions::from_mode(0o744)).expect("mode");
	let inventory = fixture.take();
	assert_eq!(present(&inventory)[0].1, InputKind::TreeExecutable);
	assert_ne!(*inventory.digest(), plain, "gaining +x with the same bytes asks again");
}

#[cfg(unix)]
#[test]
fn tree_links_inside_the_repository_are_followed_and_escapes_fail_closed() {
	use std::os::unix::fs::symlink;

	let fixture = fixture();
	write(&fixture.repo.join("shared/ext/omp.toml"), "id = 'shared'");
	write(&fixture.repo.join("shared/tool.py"), "print(1)");
	fs::create_dir_all(fixture.repo.join(EXTENSIONS_DIR)).expect("extensions");
	fs::create_dir_all(fixture.repo.join(".agents/plugins")).expect("plugins");
	symlink("../../shared/ext", fixture.repo.join(".omp/extensions/linked"))
		.expect("directory link");
	symlink("../tool.py", fixture.repo.join("shared/ext/tool.py")).expect("file link");
	// `up` leads back to `shared`, whose `ext` is being walked: that cycle is
	// not followed again. A dangling link is absent.
	symlink("..", fixture.repo.join("shared/ext/up")).expect("cycle link");
	symlink("missing", fixture.repo.join("shared/ext/gone")).expect("dangling link");
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [
		(".omp/extensions/linked/omp.toml".to_owned(), InputKind::TreeFile),
		(".omp/extensions/linked/tool.py".to_owned(), InputKind::TreeFile),
		(".omp/extensions/linked/up/tool.py".to_owned(), InputKind::TreeFile),
	]);
	let digest = *inventory.digest();
	write(&fixture.repo.join("shared/tool.py"), "print(2)");
	assert_ne!(fixture.digest(), digest, "the link target's content is bound");

	write(&fixture.outside.join("evil/omp.toml"), "id = 'evil'");
	symlink(fixture.outside.join("evil"), fixture.repo.join(".agents/plugins/evil"))
		.expect("escaping link");
	assert!(matches!(
		fixture.error(),
		WorkspaceTrustError::Escapes { path } if path == fixture.repo.join(".agents/plugins/evil")
	));
	fs::remove_file(fixture.repo.join(".agents/plugins/evil")).expect("unlink");
	symlink(fixture.outside.join("evil/omp.toml"), fixture.repo.join(".agents/plugins/f.toml"))
		.expect("escaping file link");
	assert!(matches!(fixture.error(), WorkspaceTrustError::Escapes { .. }));
	fs::remove_file(fixture.repo.join(".agents/plugins/f.toml")).expect("unlink");

	// A gated root reached through a link out of the repository.
	write(&fixture.outside.join("plugins/p/omp.toml"), "id = 'p'");
	fs::remove_dir_all(fixture.repo.join(".agents")).expect("remove");
	symlink(&fixture.outside, fixture.repo.join(".agents")).expect("root link");
	assert!(matches!(fixture.error(), WorkspaceTrustError::Escapes { .. }));
	fs::remove_file(fixture.repo.join(".agents")).expect("unlink");

	// A fixed file linked out of the repository is refused by the reader.
	symlink(fixture.outside.join("evil/omp.toml"), fixture.repo.join(ROOT_MCP_FILE))
		.expect("fixed link");
	assert!(matches!(
		fixture.error(),
		WorkspaceTrustError::Input(error) if error.refusal() == Some(Refusal::OutsideRoot)
	));
}

#[cfg(unix)]
#[test]
fn special_files_are_refused_without_blocking() {
	use std::{process::Command, sync::mpsc, thread, time::Duration};

	for fifo in [".omp/mcp.json", ".omp/extensions/x/pipe", ".omp/workflows/w.toml"] {
		let fixture = fixture();
		let path = fixture.repo.join(fifo);
		fs::create_dir_all(path.parent().expect("parent")).expect("parent");
		assert!(
			Command::new("mkfifo")
				.arg(&path)
				.status()
				.expect("mkfifo")
				.success()
		);
		let (sender, receiver) = mpsc::channel();
		thread::spawn(move || {
			let _ = sender.send(fixture.take_with(&InventoryBudget::default()).map(drop));
		});
		let result = receiver
			.recv_timeout(Duration::from_secs(10))
			.expect("a FIFO must be refused, not read");
		assert!(
			matches!(
				&result,
				Err(WorkspaceTrustError::Input(error)) if error.refusal() == Some(Refusal::NotRegular)
			),
			"{fifo}: {result:?}"
		);
	}
}

#[test]
fn enabled_plugin_install_roots_follow_where_they_live() {
	let fixture = fixture();
	// Inside the repository: walked under its path from the workspace.
	let vendored = fixture.repo.join("vendor/plugin");
	write(&vendored.join(".claude-plugin/plugin.json"), "{}");
	write(&vendored.join("skills/s/SKILL.md"), "skill");
	// In the user plugin cache: the operator materialized it.
	let cached = plugin_cache_dir(&user_plugins_dir(&fixture.data)).join("m___cached___1");
	write(&cached.join("skills/s/SKILL.md"), "skill");
	fixture.register(&[("vendored@m", &vendored, true), ("cached@m", &cached, true)]);
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [
		(".omp/plugins/installed_plugins.json".to_owned(), InputKind::Bytes),
		("vendor/plugin/.claude-plugin/plugin.json".to_owned(), InputKind::TreeFile),
		("vendor/plugin/skills/s/SKILL.md".to_owned(), InputKind::TreeFile),
	]);
	let digest = *inventory.digest();
	write(&cached.join("skills/s/SKILL.md"), "changed in the cache");
	assert_eq!(fixture.digest(), digest, "the user plugin cache is not the workspace's");
	write(&vendored.join("skills/s/SKILL.md"), "changed in the repository");
	assert_ne!(fixture.digest(), digest, "an in-repository plugin's content is bound");

	// Anywhere else: fail closed, unless the install never loads.
	let external = fixture.outside.join("plugin");
	write(&external.join("skills/s/SKILL.md"), "skill");
	fixture.register(&[("external@m", &external, true)]);
	assert!(matches!(
		fixture.error(),
		WorkspaceTrustError::ExternalPluginRoot { plugin, path }
			if plugin.as_str() == "external@m" && path == external
	));
	fixture.register(&[
		("external@m", &external, false),
		("no-marketplace", &external, true),
		("missing@m", &fixture.outside.join("missing"), true),
		("file@m", &external.join("skills/s/SKILL.md"), true),
	]);
	fixture.take();
}

#[test]
fn a_relative_install_path_resolves_like_plugin_resolution() {
	let fixture = fixture();
	let vendored = fixture.repo.join("vendor/relative");
	write(&vendored.join("commands/c.md"), "command");
	// Relative to the process working directory, as `ClaudePlugins::resolve`
	// opens it.
	let cwd = fs::canonicalize(env::current_dir().expect("cwd")).expect("canonical cwd");
	let relative = cwd
		.components()
		.skip(1)
		.map(|_| Component::ParentDir)
		.chain(vendored.components().skip(1))
		.collect::<PathBuf>();
	assert!(relative.is_relative());
	fixture.register(&[("relative@m", &relative, true)]);
	let inventory = fixture.take();
	assert!(
		present(&inventory)
			.contains(&("vendor/relative/commands/c.md".to_owned(), InputKind::TreeFile)),
		"{:?}",
		present(&inventory)
	);
	let digest = *inventory.digest();
	write(&vendored.join("commands/c.md"), "edited");
	assert_ne!(fixture.digest(), digest);
}

#[cfg(unix)]
#[test]
fn a_project_scope_install_link_does_not_block_trust() {
	use std::os::unix::fs::symlink;

	let fixture = fixture();
	// `omp ext install --scope project`: the registry names the cache, and
	// `.omp/plugins/node_modules/<name>` links to it.
	let cached = plugin_cache_dir(&user_plugins_dir(&fixture.data)).join("m___tool___1");
	write(&cached.join("hooks/hooks.json"), "{}");
	fixture.register(&[("tool@m", &cached, true)]);
	let node_modules = fixture.repo.join(PROJECT_PLUGINS_DIR).join("node_modules");
	fs::create_dir_all(&node_modules).expect("node_modules");
	symlink(&cached, node_modules.join("tool")).expect("install link");
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [(
		".omp/plugins/installed_plugins.json".to_owned(),
		InputKind::Bytes
	)]);
}

#[test]
fn a_plugin_root_outside_a_nested_workspace_is_named_from_the_workspace() {
	let fixture = fixture();
	let workspace = fixture.repo.join("crates/app");
	fs::create_dir_all(&workspace).expect("nested workspace");
	let vendored = fixture.repo.join("vendor/p");
	write(&vendored.join("rules/r.md"), "rule");
	let mut registry = InstalledPluginsRegistry::default();
	registry
		.plugins
		.insert(Str::new_static("p@m"), vec![InstalledPluginEntry {
			scope:          InstallScope::Project,
			install_path:   vendored,
			version:        Str::new_static("1"),
			installed_at:   Str::new_static("t"),
			last_updated:   Str::new_static("t"),
			git_commit_sha: None,
			enabled:        true,
		}]);
	write(
		&project_plugins_dir(&workspace).join(REGISTRY_FILE),
		serde_json::to_vec(&registry).expect("registry"),
	);
	let inventory =
		inventory(&workspace, &fixture.data, &InventoryBudget::default()).expect("inventory");
	assert!(
		present(&inventory).contains(&("../../vendor/p/rules/r.md".to_owned(), InputKind::TreeFile))
	);
}

#[test]
fn budgets_fail_closed() {
	let fixture = fixture();
	fixture.write(".omp/extensions/a/one.py", "1");
	fixture.write(".omp/extensions/a/two.py", "22");
	fixture.write(".omp/extensions/a/deep/er/three.py", "333");
	let budget = InventoryBudget::default();
	fixture.take_with(&budget).expect("within budget");
	for (budget, limit) in [
		(InventoryBudget { tree_files: 2, ..budget }, BudgetLimit::Files),
		(InventoryBudget { tree_bytes: 5, ..budget }, BudgetLimit::Bytes),
		(InventoryBudget { depth: 2, ..budget }, BudgetLimit::Depth),
	] {
		assert!(
			matches!(
				fixture.take_with(&budget),
				Err(WorkspaceTrustError::Budget { limit: overrun, .. }) if overrun == limit
			),
			"{limit}"
		);
	}
	for (budget, what) in [
		(InventoryBudget { tree_file_bytes: 2, ..budget }, "a tree file"),
		(InventoryBudget { file_bytes: 1, ..budget }, "a fixed file"),
	] {
		fixture.write(ROOT_MCP_FILE, "{}");
		assert!(
			matches!(
				fixture.take_with(&budget),
				Err(WorkspaceTrustError::Input(error)) if error.refusal() == Some(Refusal::TooLarge)
			),
			"{what} over its cap"
		);
	}
	// Every bound is inclusive: an exact fit passes.
	let exact = InventoryBudget { tree_files: 3, tree_bytes: 6, depth: 2, ..budget };
	assert!(matches!(
		fixture.take_with(&exact),
		Err(WorkspaceTrustError::Budget { limit: BudgetLimit::Depth, .. })
	));
	fixture
		.take_with(&InventoryBudget { depth: 3, ..exact })
		.expect("an exact fit passes");
}

#[test]
fn present_entries_are_what_an_untrusted_workspace_withholds() {
	let fixture = fixture();
	fixture.write(".omp/SYSTEM.md", "be terse");
	fixture.write(".omp/secrets.yml", "[]");
	let inventory = fixture.take();
	assert_eq!(present(&inventory), [
		(".omp/SYSTEM.md".to_owned(), InputKind::Bytes),
		(".omp/secrets.yml".to_owned(), InputKind::Bytes),
	]);
	let hashes = inventory
		.present()
		.map(|entry| *entry.hash().expect("present entries are hashed"))
		.collect::<Vec<_>>();
	assert_eq!(hashes, [Hash32::sum("be terse"), Hash32::sum("[]")]);
	assert!(
		inventory
			.entries()
			.filter(|entry| entry.kind() == InputKind::Absent)
			.all(|entry| entry.hash().is_none())
	);
}
