//! The real `omp_buildbuddy` extension must seal and project its dynamic
//! `exec` parent into a runnable CONTROL row.
//!
//! The extension declares one soft tool `exec@.1` in its manifest and registers
//! `omp.devices.parent("exec", family="buildbuddy", rev=1)` at import. The Rust
//! side admits a dynamic mount only when that parent is in the sealed
//! registration evidence, and that evidence comes from
//! `DeclarationRegistry.worker_tool_definitions()`. No other test seals the
//! real extension, so this is the only place a parent that cannot be projected
//! would show.

use std::ffi::CString;

use omp_py::Engine;

const EXTENSION_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/buildbuddy/src");

const SCRIPT: &str = r#"
import sys

sys.path.insert(0, "@EXTENSION_SRC@")

import omp
import omp._registry as registry_module

omp.packages._install_snapshot(
    [{
        "name": "omp-buildbuddy",
        "version": "0.1.0",
        "extension_id": "omp.buildbuddy",
        "root": "/omp-buildbuddy",
        "files": (),
    }],
    own="omp.buildbuddy",
)
# The two rows of extensions/buildbuddy/omp.toml, in the form the host hands to
# `configure_manifest`: the soft tool `exec@.1` and the `extension_activate`
# observer hook.
registry_module.configure_manifest(
    extension="omp.buildbuddy",
    tools=(("exec", "", 1),),
    hooks=(("extension_activate", "observe"),),
    capabilities=("env.net", "env.net.fetch"),
)

step = "import omp_buildbuddy"
try:
    import omp_buildbuddy  # noqa: F401  (registers EXEC_PARENT and the hook)

    step = "freeze_declarations"
    registry_module.freeze_declarations()

    step = "worker_tool_definitions"
    rows = registry_module.registry.worker_tool_definitions()
except BaseException as error:
    raise AssertionError(
        f"the real buildbuddy extension fails at `{step}`: "
        f"{type(error).__name__}: {error}"
    ) from error

exec_rows = [row for row in rows if row.name == "exec"]
assert len(exec_rows) == 1, [(row.name, row.family, row.rev) for row in rows]
assert exec_rows[0].family == "buildbuddy" and exec_rows[0].rev == 1, exec_rows[0]
"#;

#[test]
fn the_real_buildbuddy_exec_parent_projects_to_a_control_row() {
	let script = CString::new(SCRIPT.replace("@EXTENSION_SRC@", EXTENSION_SRC))
		.expect("script has no NUL byte");
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| py.run(script.as_c_str(), None, None))
		.expect("the real buildbuddy extension seals and projects its dynamic parent");
}
