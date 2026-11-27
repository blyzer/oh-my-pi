//! Every in-tree extension that registers a dynamic device parent must seal
//! and project it into a runnable CONTROL row.
//!
//! An extension declares its tool in `omp.toml` and registers
//! `omp.devices.parent(...)` at import. The Rust side admits a dynamic mount
//! only when that parent is in the sealed registration evidence, and that
//! evidence comes from `DeclarationRegistry.worker_tool_definitions()`. No
//! other test seals the real extensions, so this is the only place a parent
//! that cannot be projected would show.
//!
//! Each case reads the rows of the extension's own `omp.toml`, so a manifest
//! that names a different identity than the code registers fails here.

use std::ffi::CString;

use omp_py::Engine;

const SCRIPT: &str = r#"
import sys

sys.path.insert(0, "@EXTENSION_ROOT@/src")

import importlib
import tomllib

import omp
import omp._registry as registry_module
from omp.devices import Effects, FetchEffects

expected_effects = @EXPECTED_EFFECTS@

with open("@EXTENSION_ROOT@/omp.toml", "rb") as handle:
    manifest = tomllib.load(handle)

omp.packages._install_snapshot(
    [{
        "name": manifest["id"],
        "version": manifest["version"],
        "extension_id": manifest["id"],
        "root": "/extension",
        "files": (),
    }],
    own=manifest["id"],
)
# The rows of omp.toml in the form the host hands to `configure_manifest`: a
# `soft` or `hard` row is a tool `name@family.rev`, and a `hook` row is
# `event/phase`.
tools, hooks = [], []
for row in manifest["declarations"]:
    if row["kind"] in ("soft", "hard"):
        name, tail = row["key"].rsplit("@", 1)
        family, rev = tail.rsplit(".", 1)
        tools.append((name, family, int(rev)))
    elif row["kind"] == "hook":
        event, phase = row["key"].split("/", 1)
        hooks.append((event, phase))
assert tools, manifest["declarations"]
registry_module.configure_manifest(
    extension=manifest["id"],
    tools=tuple(tools),
    hooks=tuple(hooks),
    capabilities=tuple(manifest.get("capabilities", ())),
    declarations=tuple(manifest["declarations"]),
)

step = "import " + manifest["entry"]
try:
    importlib.import_module(manifest["entry"])  # registers the parent and its hook

    step = "freeze_declarations"
    registry_module.freeze_declarations()

    step = "worker_tool_definitions"
    rows = registry_module.registry.worker_tool_definitions()
except BaseException as error:
    raise AssertionError(
        f"{manifest['id']} fails at `{step}`: {type(error).__name__}: {error}"
    ) from error

# CONTROL (`seal_tool`) requires each row to match its manifest declaration: the
# same kind and module, and a `{name}@{family}.{rev}` key present in the
# manifest. A sub-module such as the one where the parent is created would be
# refused.
for declared in manifest["declarations"]:
    if declared["kind"] not in ("soft", "hard"):
        continue
    matching = [
        row
        for row in rows
        if f"{row.name}@{row.family}.{row.rev}" == declared["key"]
    ]
    assert len(matching) == 1, (declared, [(r.name, r.family, r.rev) for r in rows])
    row = matching[0]
    assert row.kind == declared["kind"], (row.kind, declared)
    assert row.source_module == declared["module"], (row.source_module, declared)
    assert str(row.place) == "host", row.place
    # What the parent declares reaches the row: every leaf below it runs under
    # this envelope, and the DATA gate needs `env.net` in it.
    assert row.effects == expected_effects, (row.effects, expected_effects)
"#;

fn seal_and_project(extension: &str, expected_effects: &str) {
	let root = format!("{}/../../extensions/{extension}", env!("CARGO_MANIFEST_DIR"));
	let script = CString::new(
		SCRIPT
			.replace("@EXTENSION_ROOT@", &root)
			.replace("@EXPECTED_EFFECTS@", expected_effects),
	)
	.expect("script has no NUL byte");
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| py.run(script.as_c_str(), None, None))
		.unwrap_or_else(|error| {
			panic!("the real {extension} extension seals and projects its dynamic parent: {error}")
		});
}

#[test]
fn the_real_buildbuddy_exec_parent_projects_to_a_control_row() {
	// Every leaf calls the Run API with `http_post` and a placement credential.
	seal_and_project("buildbuddy", "Effects(fetch=FetchEffects(credentials=True))");
}

#[test]
fn the_real_ci_parent_projects_to_a_control_row() {
	seal_and_project("ci", "None");
}

#[test]
fn the_real_forge_parent_projects_to_a_control_row() {
	seal_and_project("forge", "None");
}

#[test]
fn a_dynamic_parent_rejects_effects_that_are_not_effects() {
	let script = c"
import omp

try:
    omp.devices.parent('probe', family='probe', rev=1, effects={'fetch': True})
except TypeError as error:
    # Not the unexpected-keyword TypeError that a parent without the
    # parameter would also raise.
    assert 'must be omp.Effects or None' in str(error), error
else:
    raise AssertionError('a value that is not omp.Effects was accepted')
";
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| py.run(script, None, None))
		.expect("a dynamic parent validates its effects");
}
