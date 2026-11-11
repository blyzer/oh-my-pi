"""Pure affected-path / command / digest helpers (no live omp host)."""

from __future__ import annotations

import fnmatch
import hashlib
import re
import shlex
from collections.abc import Mapping, Sequence

_MAX_PATHS = 256
_MAX_PATH_BYTES = 1_024
_MAX_COMMAND_BYTES = 16_384
_MAX_SUMMARY_BYTES = 3_072
_ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_VOLATILE_DURATION = re.compile(r"\b\d+(?:\.\d+)?(?:ms|s)\b")
_PATCH_HEADER = re.compile(r"^\[([^#\]\r\n]+)#[0-9A-Fa-f]{4}\]$")

DEFAULT_AFFECTED: Mapping[str, Mapping[str, object]] = {
	"node": {
		"patterns": ("*.js", "*.jsx", "*.mjs", "*.cjs", "*.ts", "*.tsx"),
		"command": "npm test -- --findRelatedTests {paths}",
	},
	"python": {
		"patterns": ("*.py",),
		"command": "python3 -m pytest {paths}",
	},
	"go": {"patterns": ("*.go",), "command": "go test ./..."},
	"rust": {"patterns": ("*.rs",), "command": "cargo test"},
	"make": {"patterns": ("Makefile", "*.mk"), "command": "make test"},
}


def bounded(text: str, maximum: int) -> str:
	encoded = text.encode("utf-8")
	if len(encoded) <= maximum:
		return text
	return encoded[-maximum:].decode("utf-8", errors="ignore")


def clean_path(value: object) -> str | None:
	if not isinstance(value, str):
		return None
	path = value.strip()
	if (
		not path
		or path.startswith(("/", "artifact://", "http://", "https://"))
		or "\x00" in path
		or len(path.encode("utf-8")) > _MAX_PATH_BYTES
	):
		return None
	return path.removeprefix("./")


def paths_in_patch(patch: object) -> tuple[str, ...]:
	if not isinstance(patch, str):
		return ()
	paths: list[str] = []
	for line in patch.splitlines():
		header = _PATCH_HEADER.fullmatch(line)
		if header:
			paths.append(header.group(1))
			continue
		if line.startswith("MV "):
			try:
				moved = shlex.split(line[3:])
			except ValueError:
				continue
			if len(moved) == 1:
				paths.append(moved[0])
	return tuple(paths)


def collect_touched_paths(
	args: Mapping[str, object] | None,
	payload: Mapping[str, object] | None,
) -> tuple[str, ...]:
	"""Fold path-bearing keys from a tool call args/payload pair."""

	values: list[object] = []
	for source in (args or {}, payload or {}):
		for key in ("path", "previous_path"):
			if key in source:
				values.append(source[key])
		for key in ("paths", "changed", "created"):
			candidate = source.get(key)
			if isinstance(candidate, Sequence) and not isinstance(candidate, (str, bytes)):
				values.extend(candidate)
		values.extend(paths_in_patch(source.get("patch")))

	paths = {
		path
		for value in values
		if (path := clean_path(value)) is not None
	}
	return tuple(sorted(paths))[:_MAX_PATHS]


def configured_mapping(settings: Mapping[str, object]) -> Mapping[str, object]:
	affected = settings.get("affected")
	return affected if isinstance(affected, Mapping) else DEFAULT_AFFECTED


def render_command(template: str, paths: Sequence[str]) -> str:
	joined = " ".join(shlex.quote(path) for path in paths)
	return template.replace("{paths}", joined)


def affected_command(paths: tuple[str, ...], settings: Mapping[str, object]) -> str:
	runner = settings.get("test_runner", "")
	if isinstance(runner, str) and runner.strip():
		command = render_command(runner.strip(), paths)
	else:
		commands: list[str] = []
		for value in configured_mapping(settings).values():
			if not isinstance(value, Mapping):
				continue
			patterns = value.get("patterns", ())
			template = value.get("command", "")
			if (
				not isinstance(patterns, Sequence)
				or isinstance(patterns, (str, bytes))
				or not isinstance(template, str)
				or not template.strip()
			):
				continue
			matched = tuple(
				path
				for path in paths
				if any(isinstance(pattern, str) and fnmatch.fnmatch(path, pattern) for pattern in patterns)
			)
			if not matched:
				continue
			rendered = render_command(template.strip(), matched)
			if rendered not in commands:
				commands.append(rendered)
		command = " && ".join(f"({item})" for item in commands)

	if len(command.encode("utf-8")) > _MAX_COMMAND_BYTES:
		raise ValueError("affected-test command exceeds 16 KiB")
	return command


def failure_digest(command: str, exit_code: int | None, output: str) -> str:
	stable = _VOLATILE_DURATION.sub("<duration>", _ANSI.sub("", output))
	material = f"{command}\0{exit_code}\0{bounded(stable, 16_384)}".encode()
	return hashlib.sha256(material).hexdigest()[:24]


__all__ = (
	"DEFAULT_AFFECTED",
	"_MAX_SUMMARY_BYTES",
	"affected_command",
	"bounded",
	"clean_path",
	"collect_touched_paths",
	"configured_mapping",
	"failure_digest",
	"paths_in_patch",
	"render_command",
)
