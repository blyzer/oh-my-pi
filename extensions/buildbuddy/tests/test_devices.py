"""BuildBuddy device tests — no network, no omp runtime.

The devices module imports `omp` at module level, so it cannot be loaded
without the native host. These tests verify the device source code structure
by reading the file directly, checking that each operation calls the client
module and handles errors properly.
"""

from __future__ import annotations

import ast
import asyncio
import importlib
import os
import sys
import types
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
DEVICES_PY = ROOT / "src" / "omp_buildbuddy" / "devices.py"
CLIENT_PY = ROOT / "src" / "omp_buildbuddy" / "client.py"


class DeviceSourceTests(unittest.TestCase):
    """Verify device structure by inspecting source AST."""

    def setUp(self) -> None:
        self.devices_source = DEVICES_PY.read_text(encoding="utf-8")
        self.devices_tree = ast.parse(self.devices_source, filename=str(DEVICES_PY))
        self.client_source = CLIENT_PY.read_text(encoding="utf-8")
        self.client_tree = ast.parse(self.client_source, filename=str(CLIENT_PY))

    def _find_async_func(self, name: str) -> ast.AsyncFunctionDef | None:
        for node in ast.walk(self.devices_tree):
            if isinstance(node, ast.AsyncFunctionDef) and node.name == name:
                return node
        return None

    def _find_func(self, name: str) -> ast.FunctionDef | None:
        for node in ast.walk(self.client_tree):
            if isinstance(node, ast.FunctionDef) and node.name == name:
                return node
        return None

    def test_describe_calls_client(self) -> None:
        """Verify describe device calls client.describe_capabilities()."""
        func = self._find_async_func("describe")
        self.assertIsNotNone(func, "describe should exist")
        self.assertIn("client.describe_capabilities", self.devices_source)

    def test_start_calls_client(self) -> None:
        """Verify start device calls client.start_run()."""
        self.assertIn("client.start_run", self.devices_source)

    def test_poll_calls_client(self) -> None:
        """Verify poll device calls client.poll_run()."""
        self.assertIn("client.poll_run", self.devices_source)

    def test_get_log_calls_client(self) -> None:
        """Verify get_log device calls client.get_log()."""
        self.assertIn("client.get_log", self.devices_source)

    def test_cancel_calls_client(self) -> None:
        """Verify cancel device calls client.cancel_run()."""
        self.assertIn("client.cancel_run", self.devices_source)

    def test_all_five_operations_exist(self) -> None:
        """Verify all five delegate operations are defined."""
        for name in ("describe", "start", "poll", "get_log", "cancel"):
            self.assertIsNotNone(
                self._find_async_func(name),
                f"Operation {name} should be defined",
            )

    def test_mount_exec_leaves_hook_exists(self) -> None:
        """Verify the extension_activate hook is defined."""
        self.assertIsNotNone(
            self._find_async_func("mount_exec_leaves"),
            "mount_exec_leaves hook should be defined",
        )

    def test_leaves_tuple_has_five_entries(self) -> None:
        """Verify _LEAVES contains exactly five MountSpec entries."""
        count = self.devices_source.count("MountSpec(")
        self.assertEqual(count, 5, "_LEAVES should have exactly 5 MountSpec entries")


class ClientSourceTests(unittest.TestCase):
    """Verify client module structure by inspecting source AST."""

    def setUp(self) -> None:
        self.source = CLIENT_PY.read_text(encoding="utf-8")
        self.tree = ast.parse(self.source, filename=str(CLIENT_PY))

    def _find_func(self, name: str) -> ast.FunctionDef | ast.AsyncFunctionDef | None:
        for node in ast.walk(self.tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name:
                return node
        return None

    def test_describe_capabilities_exists(self) -> None:
        self.assertIsNotNone(self._find_func("describe_capabilities"))

    def test_start_run_exists(self) -> None:
        self.assertIsNotNone(self._find_func("start_run"))

    def test_poll_run_exists(self) -> None:
        self.assertIsNotNone(self._find_func("poll_run"))

    def test_get_log_exists(self) -> None:
        """Verify get_log function exists for log fetching."""
        self.assertIsNotNone(self._find_func("get_log"))

    def test_cancel_run_exists(self) -> None:
        self.assertIsNotNone(self._find_func("cancel_run"))

    def test_uses_omp_env_http_post(self) -> None:
        """Verify client uses omp.env.http_post for HTTP calls (all endpoints are POST)."""
        self.assertIn("omp.env.http_post", self.source)

    def test_reads_http_response_status_field(self) -> None:
        """omp.env.HttpResponse exposes `status`; `status_code` does not exist and would raise."""
        self.assertIn("response.status", self.source)
        self.assertNotIn("status_code", self.source)

    def test_no_omp_env_http_get(self) -> None:
        """Verify client does not use omp.env.http_get (all endpoints are POST)."""
        self.assertNotIn("omp.env.http_get", self.source)

    def test_the_credential_is_placed_by_the_host_not_set_here(self) -> None:
        """S5: the client names the provider; it neither sets nor reads the key."""
        self.assertIn("credential=_PROVIDER", self.source)
        self.assertIn('_PROVIDER = "buildbuddy"', self.source)
        self.assertNotIn("x-buildbuddy-api-key", self.source)

    def test_no_api_key_plumbing_remains(self) -> None:
        """No api_key argument, env read or subprocess keychain lookup is left."""
        for name in ("_api_key", "api_key", "BUILDBUDDY_API_KEY", "subprocess"):
            self.assertNotIn(name, self.source, name)
        self.assertNotIn('get("api_key")', DEVICES_PY.read_text(encoding="utf-8"))

    def test_has_base_url_function(self) -> None:
        """Verify _base_url function exists for endpoint conversion."""
        self.assertIsNotNone(self._find_func("_base_url"))

    def test_cancel_returns_unsupported(self) -> None:
        """Verify cancel_run returns unsupported outcome (REST has no cancel)."""
        self.assertIn("unsupported", self.source)

    def test_uses_correct_endpoints(self) -> None:
        """Verify client uses correct BuildBuddy API endpoint names."""
        self.assertIn('"Run"', self.source)
        self.assertIn('"GetInvocation"', self.source)
        self.assertIn('"GetLog"', self.source)


class ClientBehaviourTests(unittest.TestCase):
    """Run client.py against a recording stand-in for the native `omp` host.

    The source tests above only check spelling. These drive `_request` and
    `_base_url` and look at what actually reaches `omp.env.http_post`.
    """

    def setUp(self) -> None:
        self.calls: list[dict[str, object]] = []
        self.status = 200
        self.body = b"{}"
        self.attempts: list[dict[str, object]] = []
        self.refuse = False
        outer = self

        class _Response:
            def __init__(self, status: int, body: bytes) -> None:
                self.status = status
                self.body = body

        class _PlacementUnavailable(Exception):
            """Stand-in for omp.env.PlacementUnavailable."""

        async def http_post(url, body=b"", headers=None, redirects=10, credential=None):
            attempt = {
                "url": url,
                "body": body,
                "headers": headers,
                "redirects": redirects,
                "credential": credential,
            }
            outer.attempts.append(attempt)
            if outer.refuse:
                # What a host with no grant does: nothing is sent.
                raise _PlacementUnavailable("no grant")
            outer.calls.append(attempt)
            return _Response(outer.status, outer.body)

        omp_mod = types.ModuleType("omp")
        env_mod = types.ModuleType("omp.env")
        env_mod.http_post = http_post
        env_mod.HttpResponse = _Response
        env_mod.PlacementUnavailable = _PlacementUnavailable
        omp_mod.env = env_mod
        modules = mock.patch.dict(sys.modules, {"omp": omp_mod, "omp.env": env_mod})
        modules.start()
        self.addCleanup(modules.stop)
        environ = mock.patch.dict(os.environ)
        environ.start()
        self.addCleanup(environ.stop)
        os.environ.pop("SV_EXEC_REMOTE_ENDPOINT", None)
        # An empty package pointing at the source directory: importing the real
        # `omp_buildbuddy/__init__.py` would load `devices`, which needs the
        # native host. Only `client`, `errors` and `profile` are exercised.
        package = types.ModuleType("omp_buildbuddy")
        package.__path__ = [str(ROOT / "src" / "omp_buildbuddy")]
        sys.modules["omp_buildbuddy"] = package
        self.client = importlib.import_module("omp_buildbuddy.client")

    def _call(self) -> object:
        return asyncio.run(self.client._request("Run", payload={}))

    def test_requests_never_follow_redirects(self) -> None:
        self._call()
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.calls[0]["redirects"], 0)

    def test_a_redirect_response_is_refused(self) -> None:
        errors = self.client.errors
        for status in (301, 302, 307, 308):
            self.status = status
            with self.assertRaises(errors.BuildBuddyUnavailable) as raised:
                self._call()
            self.assertIn("redirect refused", raised.exception.reason)

    def test_plain_http_endpoints_are_refused(self) -> None:
        errors = self.client.errors
        for endpoint in ("grpc://app.buildbuddy.io", "http://app.buildbuddy.io", "ftp://x.io", "app.buildbuddy.io"):
            os.environ["SV_EXEC_REMOTE_ENDPOINT"] = endpoint
            with self.assertRaises(errors.BuildBuddyUnavailable, msg=endpoint):
                self.client._base_url()

    def test_the_key_is_never_sent_to_a_refused_endpoint(self) -> None:
        os.environ["SV_EXEC_REMOTE_ENDPOINT"] = "grpc://app.buildbuddy.io"
        with self.assertRaises(self.client.errors.BuildBuddyUnavailable):
            self._call()
        self.assertEqual(self.calls, [])

    def test_https_and_grpcs_endpoints_are_accepted(self) -> None:
        self.assertEqual(self.client._base_url(), "https://app.buildbuddy.io")
        os.environ["SV_EXEC_REMOTE_ENDPOINT"] = "https://example.buildbuddy.io"
        self.assertEqual(self.client._base_url(), "https://example.buildbuddy.io")

    def test_the_request_names_the_provider_and_carries_no_key(self) -> None:
        os.environ["BUILDBUDDY_API_KEY"] = "envsecret"
        self._call()
        attempt = self.calls[0]
        self.assertEqual(attempt["credential"], "buildbuddy")
        self.assertNotIn("envsecret", repr(attempt))
        self.assertNotIn("x-buildbuddy-api-key", {key.lower() for key in attempt["headers"]})

    def test_a_host_that_places_nothing_means_nothing_is_sent(self) -> None:
        os.environ["BUILDBUDDY_API_KEY"] = "envsecret"
        self.refuse = True
        with self.assertRaises(self.client.errors.BuildBuddyNotGranted) as raised:
            self._call()
        self.assertEqual(self.calls, [])
        self.assertNotIn("envsecret", str(raised.exception))
        self.assertNotIn("envsecret", repr(self.attempts))

    def test_an_http_failure_is_unavailable_not_a_grant_error(self) -> None:
        self.status = 503
        with self.assertRaises(self.client.errors.BuildBuddyUnavailable) as raised:
            self._call()
        self.assertNotIsInstance(raised.exception, self.client.errors.BuildBuddyNotGranted)
        self.assertIn("503", raised.exception.reason)

    def test_start_run_succeeds_and_ignores_a_key_in_its_arguments(self) -> None:
        self.body = b'{"invocationId": "inv-1"}'
        result = asyncio.run(
            self.client.start_run({"steps": [{"run": "true"}], "api_key": "leak"})
        )
        self.assertEqual(result["operation"], "inv-1")
        self.assertNotIn("leak", repr(self.attempts))


if __name__ == "__main__":
    unittest.main()
