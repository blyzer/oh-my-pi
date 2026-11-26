//! Runs the real `omp_buildbuddy` extension against the real `omp.env` inside
//! embedded CPython (ADR 0075 S5).
//!
//! The extension's own unit tests fake the whole `omp.env` module so they run
//! on any `python3`. This one keeps the native `_omp` value types and the real
//! `omp.env.http_post`, `HttpResponse` and `PlacementUnavailable`, and replaces
//! only the DATA backend through the `_install_backend` seam, so it needs no
//! daemon, sandbox, credential store or network.

use std::ffi::CString;

use omp_py::Engine;

const EXTENSION_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/buildbuddy/src");

const SCRIPT: &str = r#"
import asyncio
import os
import sys

sys.path.insert(0, "@EXTENSION_SRC@")

import omp
import omp.env
from omp_buildbuddy import client, errors

os.environ["BUILDBUDDY_API_KEY"] = "env-secret-must-not-travel"
os.environ["SV_EXEC_REMOTE_ENDPOINT"] = "grpcs://remote.example.test"


class Backend:
    """What the host's DATA client does, minus the host."""

    def __init__(self):
        self.calls = []
        self.refuse = False
        self.status = 200
        self.body = b'{"invocationId": "inv-1"}'

    async def http_request(self, method, url, **options):
        self.calls.append((method, url, options))
        if self.refuse:
            raise omp.env.PlacementUnavailable("the host has no grant for this extension")
        return omp.env.HttpResponse(self.status, {}, self.body, url)


receipt = omp.env.EnvInfo(
    workspace_id=b"workspace", root=omp.EnvPath("file:///workspace"),
    server_epoch=b"epoch", server_version="test", server_build="build",
    schema_rev=1, capabilities=frozenset({omp.env.Capability.NET}), remote=False,
)
backend = Backend()
tokens = omp.env._install_backend(backend, receipt)


def everything_sent():
    return repr(backend.calls)


async def exercise():
    # 1. A request names the provider and carries no key of its own.
    result = await client.start_run({"steps": [{"run": "true"}], "api_key": "caller-key"})
    assert result["operation"] == "inv-1", result
    method, url, options = backend.calls[0]
    assert method == "POST", method
    assert url == "https://remote.example.test/api/v1/Run", url
    assert options["credential"] == "buildbuddy", options
    assert options["redirects"] == 0, options
    lowered = {name.lower() for name in options["headers"]}
    assert "x-buildbuddy-api-key" not in lowered, lowered
    for secret in ("env-secret-must-not-travel", "caller-key"):
        assert secret not in everything_sent(), secret

    # 2. Poll and log requests carry the same provider and no key.
    backend.body = b'{"invocation": []}'
    await client.poll_run("inv-1")
    backend.body = b'{"log": ""}'
    await client.get_log("inv-1")
    assert len(backend.calls) >= 3, backend.calls
    for _, _, call_options in backend.calls:
        assert call_options["credential"] == "buildbuddy", call_options
        assert call_options["redirects"] == 0, call_options

    # 3. A host that places nothing means the typed refusal, with no key text.
    backend.refuse = True
    before = len(backend.calls)
    try:
        await client.start_run({"steps": [{"run": "true"}]})
    except errors.BuildBuddyNotGranted as exc:
        assert "env-secret-must-not-travel" not in str(exc), str(exc)
    else:
        raise AssertionError("a refused placement must raise BuildBuddyNotGranted")
    assert len(backend.calls) == before + 1, "the refusal comes from the host's reply"

    # 4. The real PlacementUnavailable is a subclass the extension catches by name.
    assert issubclass(omp.env.PlacementUnavailable, omp.env.EnvError)

    # 5. An HTTP failure is unavailable, not a grant error.
    backend.refuse = False
    backend.status = 503
    try:
        await client.start_run({"steps": [{"run": "true"}]})
    except errors.BuildBuddyNotGranted:
        raise AssertionError("a 503 is not a missing grant")
    except errors.BuildBuddyUnavailable as exc:
        assert "503" in exc.reason, exc.reason


asyncio.run(exercise())
omp.env._reset_backend(tokens)
"#;

#[test]
fn the_real_extension_names_the_provider_and_never_carries_the_key() {
	let engine = Engine::builder().init().expect("boot embedded Python");
	let script = CString::new(SCRIPT.replace("@EXTENSION_SRC@", EXTENSION_SRC))
		.expect("script has no NUL byte");
	engine
		.attach(|py| py.run(script.as_c_str(), None, None))
		.expect("the real extension runs against the real omp.env");
}
