//! The real `remote-approve` extension must register its approver and hooks
//! from its own entry module, and its manifest must declare those hooks.
//!
//! `omp.toml` names `entry = "omp_remote_approve"`. The approver and the two
//! hooks are defined in `omp_remote_approve.device`, which the entry module did
//! not import, so the host loaded the package and registered nothing: no
//! approver was offered Core's tickets and the poll hooks never ran. The pure
//! unit tests of the package never import the entry under a frozen `omp`, so
//! none of them could show it.
//!
//! This test reads the extension's own `omp.toml`, imports its own entry
//! module the way the host does, seals the registry, and then checks what was
//! registered. Sealing also refuses a hook the code registers and the manifest
//! does not declare, so the manifest and the code cannot drift apart here.

use std::ffi::CString;

use omp_py::Engine;

const SCRIPT: &str = r#"
import sys

sys.path.insert(0, "@EXTENSION_ROOT@/src")

import importlib
import tomllib

import omp
import omp._registry as registry_module

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
# `hook` row is `event/phase`.
hooks = []
for row in manifest.get("declarations", ()):
    if row["kind"] == "hook":
        event, phase = row["key"].split("/", 1)
        hooks.append((event, phase))
registry_module.configure_manifest(
    extension=manifest["id"],
    tools=(),
    hooks=tuple(hooks),
    capabilities=tuple(manifest.get("capabilities", ())),
    declarations=tuple(manifest.get("declarations", ())),
)

step = "import " + manifest["entry"]
try:
    importlib.import_module(manifest["entry"])

    step = "freeze_declarations"
    registry_module.freeze_declarations()
except BaseException as error:
    raise AssertionError(
        f"{manifest['id']} fails at `{step}`: {type(error).__name__}: {error}"
    ) from error

approvers = registry_module.registry.approver_definitions()
assert [definition.name for definition in approvers] == ["remote-approve"], approvers

# The two hooks `device.py` registers: the one that upserts the poll schedule
# when the extension activates, and the one that polls on each turn start.
declared = sorted(hooks)
assert declared == [
    ("extension_activate", "observe"),
    ("turn_start", "observe"),
], declared
"#;

#[test]
fn the_real_remote_approve_entry_registers_its_approver_and_declares_its_hooks() {
	let root = format!("{}/../../extensions/remote-approve", env!("CARGO_MANIFEST_DIR"));
	let script =
		CString::new(SCRIPT.replace("@EXTENSION_ROOT@", &root)).expect("script has no NUL byte");
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| py.run(script.as_c_str(), None, None))
		.unwrap_or_else(|error| {
			panic!("the real remote-approve extension registers its approver and hooks: {error}")
		});
}
