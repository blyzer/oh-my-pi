"""BuildBuddy Run API HTTP client.

Uses `omp.env.http_post` to call BuildBuddy's Run API REST endpoints. All
endpoints are POST-based JSON calls routed through the sandbox's HTTP egress
broker, which checks each destination against `sv_sandbox_allow_domains`. The
API key is never an argument and never reaches this process: every request
names `credential="buildbuddy"` and the host places the header at egress under
a grant (ADR 0075). A host that does not place it refuses before sending.

Run API endpoints (from BuildBuddy Enterprise API docs):
- `POST /api/v1/Run` — submit a run, returns invocation ID
- `POST /api/v1/GetInvocation` — get invocation status by invocation ID
- `POST /api/v1/GetLog` — page invocation log output by invocation ID

Cancel is gRPC-only (`BuildBuddyService.CancelExecutions`); the REST API has
no cancel endpoint. The extension reports `CancelSupport::None`.
"""

from __future__ import annotations

import json
import os
from typing import Any

import omp
from omp.env import HttpResponse

from . import errors
from .profile import ENDPOINT


# The provider whose credential the host places on each request (catalog id).
_PROVIDER = "buildbuddy"


def _base_url() -> str:
    """Return the BuildBuddy REST base URL from the gRPC endpoint.

    Converts `grpcs://app.buildbuddy.io` → `https://app.buildbuddy.io`. The API
    key is sent to this URL, so anything but https is refused: a plain `grpc://`
    or `http://` endpoint would put the key on the wire in the clear.
    """
    endpoint = os.environ.get("SV_EXEC_REMOTE_ENDPOINT", ENDPOINT)
    if endpoint.startswith("grpcs://"):
        endpoint = endpoint.replace("grpcs://", "https://", 1)
    if not endpoint.startswith("https://"):
        raise errors.BuildBuddyUnavailable(
            "auth", "BuildBuddy endpoint must be https: the API key is sent to it"
        )
    return endpoint


def _headers() -> dict[str, str]:
    """Build the request headers. The credential is not among them: the host places it."""
    return {
        "Content-Type": "application/json",
        "User-Agent": "omp-buildbuddy/0.1",
    }


async def _request(
    endpoint: str, *, payload: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Execute one BuildBuddy REST API call through the sandbox egress broker.

    All BuildBuddy REST endpoints are POST-based JSON calls. The request carries
    `credential="buildbuddy"`: the host places the API key at egress, so this
    module never holds it. If the host does not place it (no grant, or a remote
    Environment) the request is refused before anything is sent and
    `BuildBuddyNotGranted` is raised. Raises `BuildBuddyUnavailable` on HTTP
    errors or network failures.
    """
    base = _base_url()
    url = base.rstrip("/") + f"/api/v1/{endpoint}"
    headers = _headers()

    body_bytes = b""
    if payload is not None:
        body_bytes = json.dumps(payload).encode("utf-8")

    response: HttpResponse
    try:
        # redirects=0: a redirect is refused below rather than followed.
        # credential: the host places the API key; it never reaches this process.
        response = await omp.env.http_post(
            url,
            body=body_bytes,
            headers=headers,
            redirects=0,
            credential=_PROVIDER,
        )
    except omp.env.PlacementUnavailable as exc:
        raise errors.BuildBuddyNotGranted(endpoint) from exc
    except Exception as exc:
        raise errors.BuildBuddyUnavailable(
            endpoint, f"BuildBuddy host unreachable: {exc}"
        ) from exc

    if 300 <= response.status < 400:
        raise errors.BuildBuddyUnavailable(
            endpoint, f"BuildBuddy HTTP {response.status} redirect refused"
        )
    if response.status >= 400:
        raise errors.BuildBuddyUnavailable(
            endpoint, f"BuildBuddy HTTP {response.status}"
        )

    body = response.body
    if not body:
        return {}
    return json.loads(body.decode("utf-8", errors="replace"))


def describe_capabilities() -> dict[str, Any]:
    """Call `describe` — return BuildBuddy backend capabilities.

    Returns a capabilities map indicating Run API features:
    workspace mirroring, runner script support, and snapshot preservation.
    Cancel is not supported via REST — only gRPC, which requires a native
    wheel not yet available.
    """
    # The Run API capabilities are static — they don't change per-request.
    # We verify connectivity by checking the endpoint, but the capabilities
    # themselves are known from the extension's profile data.
    return {
        "capabilities": {
            "workspace_mirroring": True,
            "runner_script": True,
            "snapshot_preservation": True,
            "cancel": False,
        },
        "backend": "buildbuddy",
        "endpoint": _base_url(),
    }


async def start_run(request: dict[str, Any]) -> dict[str, Any]:
    """Call `start` — submit a run via `POST /api/v1/Run`.

    The BuildBuddy Run API expects a request with:
    - `steps`: list of step objects with `run` commands to execute
    - `repo`: optional git repo URL for workspace cloning
    - `branch`: optional branch name
    - `commit_sha`: optional commit SHA
    - `patches`: optional Base64-encoded git patches
    - `env`: optional environment variables map
    - `wait_until`: optional, defaults to returning after dispatch

    Returns the invocation ID assigned by BuildBuddy.
    """
    steps = request.get("steps", [])
    if not steps:
        command = request.get("command", "")
        if command:
            steps = [{"run": command}]

    payload: dict[str, Any] = {"steps": steps}

    # Optional workspace fields
    repo = request.get("repo", "")
    if repo:
        payload["repo"] = repo
    branch = request.get("branch", "")
    if branch:
        payload["branch"] = branch
    commit_sha = request.get("commit_sha", "")
    if commit_sha:
        payload["commit_sha"] = commit_sha
    patches = request.get("patches", [])
    if patches:
        payload["patches"] = patches
    env = request.get("env", {})
    if env:
        payload["env"] = env

    result = await _request("Run", payload=payload)
    invocation_id = result.get("invocationId") or result.get("invocation_id") or ""
    if not invocation_id:
        raise errors.BuildBuddyUnavailable("Run", "no invocation ID in response")
    return {
        "operation": invocation_id,
        "backend": "buildbuddy",
    }


async def poll_run(
    operation: str, cursor: str | None = None
) -> dict[str, Any]:
    """Call `poll` — get invocation status via `POST /api/v1/GetInvocation`.

    BuildBuddy's GetInvocation endpoint returns invocation metadata including
    success status, duration, and exit code. Uses pagination via
    `next_page_token` for log output.

    Returns the execution state, output page, and next cursor.
    """
    payload: dict[str, Any] = {"selector": {"invocation_id": operation}}
    if cursor:
        payload["page_token"] = cursor

    result = await _request("GetInvocation", payload=payload)

    # BuildBuddy returns `invocation` as a list; take the first item
    invocations = result.get("invocation", [])
    if not invocations:
        return {
            "state": "unknown",
            "output": "",
            "nextCursor": "",
        }

    invocation = invocations[0]

    # Map BuildBuddy's response to the delegate contract shape
    success = invocation.get("success")
    if success is True:
        state = "succeeded"
    elif success is False:
        state = "failed"
    else:
        duration = invocation.get("durationUsec", "")
        if duration:
            state = "running"
        else:
            state = "unknown"

    next_cursor = result.get("next_page_token", "")

    return {
        "state": state,
        "output": "",
        "nextCursor": next_cursor,
    }


async def get_log(
    operation: str, cursor: str | None = None
) -> dict[str, Any]:
    """Call `get_log` — fetch invocation logs via `POST /api/v1/GetLog`.

    BuildBuddy's GetLog endpoint returns log chunks for an invocation.
    Logs are streamed live and can be fetched in parallel with the run.

    Returns log output and the next page token for pagination.
    """
    payload: dict[str, Any] = {"selector": {"invocation_id": operation}}
    if cursor:
        payload["page_token"] = cursor

    result = await _request("GetLog", payload=payload)

    # BuildBuddy returns log chunks in the response
    chunks = result.get("chunks", [])
    output_parts = []
    for chunk in chunks:
        data = chunk.get("data", "")
        if data:
            output_parts.append(data)

    next_cursor = result.get("next_page_token", "")

    return {
        "output": "\n".join(output_parts),
        "nextCursor": next_cursor,
    }


async def cancel_run(operation: str) -> dict[str, Any]:
    """Call `cancel` — return `Unsupported` since REST API has no cancel endpoint.

    BuildBuddy's REST API has no cancel endpoint. Cancellation requires the
    gRPC `BuildBuddyService.CancelExecutions` call, which needs a native gRPC
    wheel not yet available in the extension. The action timeout bounds
    leaked remote work (30 minutes default per ADR 0040).
    """
    return {
        "outcome": "unsupported",
        "reason": "BuildBuddy REST API has no cancel endpoint; gRPC required",
    }
