#!/usr/bin/env python3
"""Records what a real ACP agent binary does, in an isolated home.

This is the evidence ADR 0043's capability gate asks for: a handshake
transcript per binary. It is opt-in and never runs in CI. It speaks raw
JSON-RPC over the agent's stdio and does not use OMP's own client, so the
transcript shows what the binary does rather than what our client expects.

    scripts/acp-probe.py --name dsh --out docs/research/acp-handshakes/dsh.transcript.md \\
        --set DSH_HOME={home}/.dsh -- /path/to/dsh --profile acp

Isolation:
  * HOME is a fresh temporary directory; the real one is never touched,
    unless --inherit-login is given (see below).
  * The environment is cleared. Only PATH, HOME, TERM and LANG are set, plus
    each --set NAME=VALUE ({home} expands to the temporary home) and each
    --pass-env NAME copied from this process. A name passed this way is
    redacted from the transcript.
  * The agent runs in its own process group and is killed at the end.

Opt-ins (both are recorded in the transcript):
  * --inherit-login runs the agent with the operator's real HOME so the
    vendor's own process uses its own existing login. Nothing is copied or
    read by this script, and no token is passed through the environment, but
    the agent can read and write its normal files there (session history).
    Use it only for a binary the operator has signed in to and has budget for.
  * --connect-only stops after initialize and session/new. No prompt is sent,
    so no model quota is spent.

Optional steps that set up and exercise the gate (all recorded):
  * --set-mode ID sends session/set_mode after session/new.
  * --config ID=VALUE (repeatable) sends session/set_config_option; VALUE is
    sent as the text given (dsh's model option takes JSON text such as
    '["deepseek-account","deepseek-flash"]').
  * --permission-prompt TEXT sends a prompt that needs a tool and records how
    many session/request_permission requests the agent made (each is
    rejected) and how the turn ended. An agent that runs the tool without
    asking shows 0 requests.

Steps (each is recorded, a failed step does not stop the run):
  initialize, session/new, session/new with a relative cwd, session/prompt
  for an unknown session, an unknown method, session/prompt with --prompt,
  session/cancel while that prompt runs, then closing stdin.

Any session/request_permission from the agent is answered with a rejection.
The probe never approves anything.
"""

import argparse
import json
import os
import queue
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

PROTOCOL_VERSION = 1

# Session, message and account identifiers all look alike, and an account id
# must not reach a committed transcript. Every UUID becomes a placeholder; the
# same UUID always maps to the same placeholder, so correlations stay visible.
UUID_RE = re.compile(
    r"(?<![0-9a-fA-F])[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(?![0-9a-fA-F])")


class Agent:
    """One agent process with a line-oriented JSON-RPC pipe."""

    def __init__(self, command, env, cwd):
        self.proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            cwd=cwd,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
        self.lines = queue.Queue()
        self.frames = []
        self.stderr = []
        self.next_id = 1
        for stream, tag in ((self.proc.stdout, "out"), (self.proc.stderr, "err")):
            threading.Thread(target=self._read, args=(stream, tag), daemon=True).start()

    def _read(self, stream, tag):
        for line in stream:
            self.lines.put((tag, line.rstrip("\n")))
        self.lines.put((tag, None))

    def send(self, message):
        text = json.dumps(message)
        self.frames.append(("->", text))
        try:
            self.proc.stdin.write(text + "\n")
            self.proc.stdin.flush()
        except OSError:
            self.frames.append(("!!", "agent stdin closed"))

    def request(self, method, params):
        request_id = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return request_id

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def wait_response(self, request_id, timeout):
        """Reads until the response for `request_id`, answering agent requests."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                tag, line = self.lines.get(timeout=0.25)
            except queue.Empty:
                if self.proc.poll() is not None:
                    return None
                continue
            if line is None:
                continue
            if tag == "err":
                self.stderr.append(line)
                continue
            self.frames.append(("<-", line))
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("id") == request_id and "method" not in message:
                return message
            if "method" in message and "id" in message:
                self._reject(message)
        return None

    def _reject(self, request):
        """Answers an agent-to-client request without granting anything."""
        if request.get("method") == "session/request_permission":
            options = request.get("params", {}).get("options", [])
            reject = next((o for o in options if str(o.get("kind", "")).startswith("reject")), None)
            outcome = (
                {"outcome": "selected", "optionId": reject["optionId"]}
                if reject
                else {"outcome": "cancelled"}
            )
            self.send({"jsonrpc": "2.0", "id": request["id"], "result": {"outcome": outcome}})
        else:
            self.send({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {"code": -32601, "message": "client does not implement this method"},
            })

    def stop(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        deadline = time.time() + 3
        while self.proc.poll() is None and time.time() < deadline:
            time.sleep(0.1)
        clean = self.proc.poll() is not None
        if not clean:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(self.proc.pid, sig)
                except ProcessLookupError:
                    break
                time.sleep(1)
                if self.proc.poll() is not None:
                    break
        return self.proc.poll(), clean


def summarize(response):
    if response is None:
        return "no response"
    if "error" in response:
        error = response["error"]
        return "error {}: {}".format(error.get("code"), str(error.get("message"))[:140])
    return "ok"


def run(args):
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        sys.exit("error: no agent command given after --")
    real_home = os.path.realpath(os.path.expanduser("~"))
    home = tempfile.mkdtemp(prefix="acp-probe-home-")
    try:
        return probe(args, command, home, real_home)
    finally:
        shutil.rmtree(home, ignore_errors=True)


def probe(args, command, home, real_home):
    if os.path.realpath(home) == real_home:
        sys.exit("error: refusing to run with the real home directory")
    work = os.path.join(home, "work")
    os.makedirs(work)

    env = {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "HOME": real_home if args.inherit_login else home,
        "TERM": "dumb",
        "LANG": "en_US.UTF-8",
    }
    for item in args.set:
        name, _, value = item.partition("=")
        env[name] = value.replace("{home}", home)
    secrets = []
    for name in args.pass_env:
        if name in os.environ:
            env[name] = os.environ[name]
            secrets.append(os.environ[name])

    version = ""
    if args.version_cmd:
        try:
            out = subprocess.run(
                shlex.split(args.version_cmd), capture_output=True, text=True, env=env,
                timeout=30, check=False,
            )
            text = (out.stdout or out.stderr).strip()
            version = text.splitlines()[-1] if text else ""
        except (OSError, subprocess.SubprocessError) as error:
            version = "version command failed: {}".format(type(error).__name__)

    agent = Agent(command, env, work)
    steps = []

    def record(name, expectation, response, note=""):
        steps.append((name, expectation, summarize(response) + (" — " + note if note else "")))

    init_id = agent.request("initialize", {
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {},
        "clientInfo": {"name": "omp-acp-probe", "version": "0"},
    })
    init = agent.wait_response(init_id, args.timeout)
    record("initialize", "result with protocolVersion", init)

    session_id = None
    if init and "result" in init:
        new_id = agent.request("session/new", {"cwd": work, "mcpServers": []})
        new = agent.wait_response(new_id, args.timeout)
        record("session/new (absolute cwd, mcpServers [])", "result with sessionId", new)
        if new and "result" in new:
            session_id = new["result"].get("sessionId")

        if session_id and args.set_mode:
            mode_id = agent.request("session/set_mode", {
                "sessionId": session_id, "modeId": args.set_mode,
            })
            record("session/set_mode {}".format(args.set_mode), "result",
                   agent.wait_response(mode_id, args.timeout))
        for item in args.config if session_id else []:
            config_id, _, value = item.partition("=")
            cfg_id = agent.request("session/set_config_option", {
                "sessionId": session_id, "configId": config_id, "value": value,
            })
            record("session/set_config_option {}".format(config_id), "result",
                   agent.wait_response(cfg_id, args.timeout))
        if session_id and args.permission_prompt:
            before = len(agent.frames)
            perm_id = agent.request("session/prompt", {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": args.permission_prompt}],
            })
            perm = agent.wait_response(perm_id, args.timeout * 3)
            asked = sum(1 for direction, text in agent.frames[before:]
                        if direction == "<-" and "session/request_permission" in text)
            note = "permission requests from the agent: {}".format(asked)
            if perm and "result" in perm:
                note += ", stopReason={}".format(perm["result"].get("stopReason"))
            if args.check_file:
                note += ", {} created: {}".format(
                    args.check_file,
                    "yes" if os.path.exists(os.path.join(work, args.check_file)) else "no")
            record("session/prompt that needs a tool",
                   "permission requests, each answered with a rejection", perm, note)

        if not args.connect_only:
            rel_id = agent.request("session/new", {"cwd": "relative/path", "mcpServers": []})
            record("session/new (relative cwd)", "error: cwd must be absolute",
                   agent.wait_response(rel_id, args.timeout))

            bad_id = agent.request("session/prompt", {
                "sessionId": "no-such-session",
                "prompt": [{"type": "text", "text": "x"}],
            })
            record("session/prompt (unknown session)", "error",
                   agent.wait_response(bad_id, args.timeout))

            unk_id = agent.request("session/does-not-exist", {})
            record("unknown method", "error -32601", agent.wait_response(unk_id, args.timeout))

            if session_id:
                prompt_id = agent.request("session/prompt", {
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": args.prompt}],
                })
                time.sleep(args.cancel_after)
                agent.notify("session/cancel", {"sessionId": session_id})
                prompt = agent.wait_response(prompt_id, args.timeout)
                note = ""
                if prompt and "result" in prompt:
                    note = "stopReason={}".format(prompt["result"].get("stopReason"))
                record("session/prompt then session/cancel after {}s".format(args.cancel_after),
                       "result with stopReason cancelled, or an error", prompt, note)

    exit_code, clean = agent.stop()
    steps.append(("close stdin", "agent exits", "exit code {}{}".format(
        exit_code, "" if clean else " (had to be killed)")))

    uuids = {}

    def redact(text):
        for secret in secrets:
            if secret:
                text = text.replace(secret, "[REDACTED]")
        text = UUID_RE.sub(
            lambda match: uuids.setdefault(match.group(0), "<uuid-{}>".format(len(uuids) + 1)), text)
        return text.replace(home, "<home>").replace(real_home, "<real-home>")

    out_lines = [
        "# ACP handshake transcript: {}".format(args.name),
        "",
        "Recorded {} by `scripts/acp-probe.py`. Raw JSON-RPC, not OMP's client.".format(
            datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")),
        "",
        "- Command: `{}`".format(redact(" ".join(command))),
        "- Version: {}".format(version or "not recorded"),
        "- Isolation: {}; passed through: {}; set: {}".format(
            "the operator's own HOME (the vendor process uses its own login; nothing copied), "
            "environment otherwise cleared" if args.inherit_login
            else "fresh temporary HOME, cleared environment",
            ", ".join(args.pass_env) or "none",
            ", ".join(item.split("=")[0] for item in args.set) or "none"),
        "- Permission requests from the agent are answered with a rejection.",
        "",
        "## Steps",
        "",
        "| Step | Expected | Observed |",
        "|---|---|---|",
    ]
    for name, expected, observed in steps:
        out_lines.append("| {} | {} | {} |".format(name, expected, redact(observed).replace("|", "\\|")))
    out_lines += ["", "## Frames", "", "```text"]
    out_lines += [redact("{} {}".format(direction, text)) for direction, text in agent.frames]
    out_lines += ["```", "", "## Agent stderr", "", "```text"]
    out_lines += [redact(line) for line in agent.stderr] or ["(empty)"]
    out_lines += ["```", ""]

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(out_lines))
    print("wrote {} ({} frames)".format(out_path, len(agent.frames)))
    for name, _, observed in steps:
        print("  {}: {}".format(name, redact(observed)))
    return 0 if init and "result" in init else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--name", required=True, help="short name of the binary, for the title")
    parser.add_argument("--out", required=True, help="transcript path (markdown)")
    parser.add_argument("--set", action="append", default=[], metavar="NAME=VALUE",
                        help="set an environment variable; {home} expands to the temporary home")
    parser.add_argument("--pass-env", action="append", default=[], metavar="NAME",
                        help="copy NAME from this process; its value is redacted from the transcript")
    parser.add_argument("--version-cmd", default="", help="command whose last output line is the version")
    parser.add_argument("--prompt", default="Reply with the single word: ok")
    parser.add_argument("--cancel-after", type=float, default=2.0, help="seconds before session/cancel")
    parser.add_argument("--timeout", type=float, default=40.0, help="seconds to wait per response")
    parser.add_argument("--inherit-login", action="store_true",
                        help="run with the operator's real HOME so the vendor process uses its own "
                             "existing login; nothing is copied, but the agent can read and write "
                             "its normal files there")
    parser.add_argument("--connect-only", action="store_true",
                        help="stop after initialize and session/new: no prompt is sent, so no model "
                             "quota is spent")
    parser.add_argument("--set-mode", default="", metavar="ID",
                        help="send session/set_mode with this mode id after session/new")
    parser.add_argument("--config", action="append", default=[], metavar="ID=VALUE",
                        help="send session/set_config_option after session/new (repeatable)")
    parser.add_argument("--permission-prompt", default="", metavar="TEXT",
                        help="a prompt that needs a tool; records the permission requests it causes")
    parser.add_argument("--check-file", default="", metavar="NAME",
                        help="a file the permission prompt asks the agent to create in the working "
                             "directory; the transcript records whether it exists afterwards")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    sys.exit(run(parser.parse_args()))


if __name__ == "__main__":
    main()
