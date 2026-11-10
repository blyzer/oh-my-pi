"""Pure finding / area / comment helpers (no live omp host)."""

from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from typing import Literal

Severity = Literal["low", "medium", "high", "critical"]

_MAX_AREAS = 8
_MAX_FINDINGS_PER_AREA = 50
_MAX_RATIONALE_CHARS = 1_000
_MAX_COMMENT_BYTES = 32_000
_AREA_RE = re.compile(r"^[a-z][a-z0-9_-]{0,31}$")
_SEVERITIES = frozenset({"low", "medium", "high", "critical"})
_SEVERITY_ORDER = {"critical": 0, "high": 1, "medium": 2, "low": 3}
_FINDING_KEYS = frozenset({"area", "severity", "path", "line", "rationale"})


@dataclass(frozen=True, slots=True)
class Finding:
	"""Represent one schema-validated review finding."""

	area: str
	severity: Severity
	path: str
	line: int
	rationale: str


@dataclass(frozen=True, slots=True)
class AreaBudget:
	"""Finite hard budget fields for one review area (host Duration applied later)."""

	area: str
	model: str
	thinking: str
	max_requests: int
	max_input_tokens: int
	max_output_tokens: int
	max_usd: float
	max_wall: str


def positive_int(value: object, field: str) -> int:
	if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
		raise ValueError(f"{field} must be a positive integer")
	return value


def positive_number(value: object, field: str) -> float:
	if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
		raise ValueError(f"{field} must be a positive number")
	return float(value)


def parse_area_budgets(raw_areas: object) -> tuple[AreaBudget, ...]:
	"""Validate ``settings.areas`` without constructing host Budget objects."""

	if not isinstance(raw_areas, Sequence) or isinstance(raw_areas, (str, bytes)):
		raise ValueError("settings.areas must be an array of tables")
	if not 1 <= len(raw_areas) <= _MAX_AREAS:
		raise ValueError(f"settings.areas must contain 1 to {_MAX_AREAS} entries")

	configs: list[AreaBudget] = []
	names: set[str] = set()
	for raw in raw_areas:
		if not isinstance(raw, Mapping):
			raise ValueError("each review area must be a table")
		area = str(raw.get("area", "")).strip()
		model = str(raw.get("model", "")).strip()
		if _AREA_RE.fullmatch(area) is None:
			raise ValueError(f"invalid review area {area!r}")
		if area in names:
			raise ValueError(f"duplicate review area {area!r}")
		if not model:
			raise ValueError(f"review area {area!r} must declare a model tier")
		thinking = str(raw.get("thinking", "med"))
		try:
			max_wall = str(raw["max_wall"])
		except KeyError as error:
			raise ValueError(f"review area {area!r} has invalid thinking or max_wall") from error
		configs.append(
			AreaBudget(
				area=area,
				model=model,
				thinking=thinking,
				max_requests=positive_int(raw.get("max_requests"), f"{area}.max_requests"),
				max_input_tokens=positive_int(
					raw.get("max_input_tokens"), f"{area}.max_input_tokens"
				),
				max_output_tokens=positive_int(
					raw.get("max_output_tokens"), f"{area}.max_output_tokens"
				),
				max_usd=positive_number(raw.get("max_usd"), f"{area}.max_usd"),
				max_wall=max_wall,
			)
		)
		names.add(area)
	return tuple(configs)


def finding_schema(area: str) -> dict[str, object]:
	return {
		"type": "object",
		"additionalProperties": False,
		"required": ["findings"],
		"properties": {
			"findings": {
				"type": "array",
				"maxItems": _MAX_FINDINGS_PER_AREA,
				"items": {
					"type": "object",
					"additionalProperties": False,
					"required": ["area", "severity", "path", "line", "rationale"],
					"properties": {
						"area": {"const": area},
						"severity": {
							"type": "string",
							"enum": ["low", "medium", "high", "critical"],
						},
						"path": {"type": "string", "minLength": 1, "maxLength": 512},
						"line": {"type": "integer", "minimum": 1},
						"rationale": {
							"type": "string",
							"minLength": 1,
							"maxLength": _MAX_RATIONALE_CHARS,
						},
					},
				},
			}
		},
	}


def clean_line(value: object, limit: int = 80) -> str:
	clean = " ".join(
		"".join(character for character in str(value) if ord(character) >= 32).split()
	)
	return clean[:limit]


def validate_ref(value: str, field: str) -> str:
	if not value or len(value) > 200 or any(ord(character) < 32 for character in value):
		raise ValueError(f"{field} must be a bounded git revision")
	return value


def parse_finding(area: str, raw: object) -> Finding:
	if not isinstance(raw, Mapping) or frozenset(raw) != _FINDING_KEYS:
		raise ValueError("finding must contain exactly the declared fields")
	raw_area = raw.get("area")
	severity = raw.get("severity")
	path = raw.get("path")
	line = raw.get("line")
	rationale = raw.get("rationale")
	if raw_area != area:
		raise ValueError("finding area does not match its reviewer")
	if severity not in _SEVERITIES:
		raise ValueError("finding severity is not declared")
	if not isinstance(path, str) or not 1 <= len(path) <= 512:
		raise ValueError("finding path is invalid")
	parts = path.replace("\\", "/").split("/")
	if path.startswith(("/", "\\")) or ".." in parts or any(ord(ch) < 32 for ch in path):
		raise ValueError("finding path must be workspace-relative")
	if isinstance(line, bool) or not isinstance(line, int) or line < 1:
		raise ValueError("finding line must be a positive integer")
	if (
		not isinstance(rationale, str)
		or not rationale.strip()
		or len(rationale) > _MAX_RATIONALE_CHARS
		or any(ord(ch) < 32 and ch not in "\t" for ch in rationale)
	):
		raise ValueError("finding rationale is invalid")
	return Finding(area, severity, path, line, rationale.strip())  # type: ignore[arg-type]


def format_comment(findings: Sequence[Finding]) -> str:
	lines = ["## Automated PR review", ""]
	for finding in findings:
		safe_path = finding.path.replace("`", "'")
		rationale = finding.rationale.replace("\r", " ").replace("\n", " ")
		lines.append(
			f"- **{finding.severity.upper()}** `{safe_path}:{finding.line}` "
			f"({finding.area}): {rationale}"
		)
	body = "\n".join(lines)
	if len(body.encode("utf-8")) > _MAX_COMMENT_BYTES:
		raise ValueError("published review comment exceeds the hard byte budget")
	return body


def sort_findings(findings: Sequence[Finding]) -> tuple[Finding, ...]:
	return tuple(
		sorted(
			findings,
			key=lambda finding: (
				_SEVERITY_ORDER[finding.severity],
				finding.path,
				finding.line,
				finding.area,
			),
		)
	)


def finding_as_dict(finding: Finding) -> dict[str, object]:
	return asdict(finding)


__all__ = (
	"AreaBudget",
	"Finding",
	"Severity",
	"_MAX_AREAS",
	"_MAX_FINDINGS_PER_AREA",
	"_SEVERITY_ORDER",
	"clean_line",
	"finding_as_dict",
	"finding_schema",
	"format_comment",
	"parse_area_budgets",
	"parse_finding",
	"positive_int",
	"positive_number",
	"sort_findings",
	"validate_ref",
)
