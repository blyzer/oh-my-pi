"""Host worker/devices for ``omp.fuzzy-index`` (needs frozen omp at import)."""

from __future__ import annotations

import json as _json
from dataclasses import asdict as _asdict
from dataclasses import dataclass as _dataclass

import omp

from .score import (
	ContentMatch,
	FileMatch,
	bounded_limit,
	fuzzy_score,
	trigrams,
	under_root,
)


@_dataclass(frozen=True, slots=True)
class FindArgs:
	"""Describe a fuzzy workspace-path query."""

	query: str
	root: str = "."
	limit: int = 50


@_dataclass(frozen=True, slots=True)
class GrepArgs:
	"""Describe a fuzzy workspace-content query."""

	query: str
	root: str = "."
	limit: int = 50
	case: bool = False


@_dataclass(frozen=True, slots=True)
class FindResult(omp.Payload):
	"""Carry typed fuzzy path matches and the indexed file count."""

	query: str
	indexed_files: int
	matches: tuple[FileMatch, ...]


@_dataclass(frozen=True, slots=True)
class GrepResult(omp.Payload):
	"""Carry typed fuzzy content matches and the indexed file count."""

	query: str
	indexed_files: int
	matches: tuple[ContentMatch, ...]


@_dataclass(frozen=True, slots=True)
class _Path:
	text: str
	grams: frozenset[str]


@_dataclass(frozen=True, slots=True)
class _Line:
	path: str
	number: int
	text: str
	grams: frozenset[str]
	exact_grams: frozenset[str]


_PATHS: tuple[_Path, ...] | None = None
_LINES: tuple[_Line, ...] | None = None


async def _read_text(path: omp.EnvPath) -> str | None:
	try:
		async with await omp.env.docs.open(path) as document:
			return await document.read()
	except (UnicodeDecodeError, omp.env.EnvError):
		return None


async def _build_index() -> None:
	global _LINES, _PATHS
	omp.env.require(omp.env.Capability.DOC_READ, omp.env.Capability.SEARCH)
	paths: list[_Path] = []
	lines: list[_Line] = []
	root = omp.env.info().root
	root_uri = root.uri.rstrip("/") + "/"
	async for entry in omp.env.find.walk(root=root):
		if entry.kind != "file":
			continue
		uri = entry.path.uri
		path = uri.removeprefix(root_uri)
		paths.append(_Path(path, trigrams(path)))
		text = await _read_text(entry.path)
		if text is None:
			continue
		lines.extend(
			_Line(
				path,
				number,
				line,
				trigrams(line),
				trigrams(line, case_sensitive=True),
			)
			for number, line in enumerate(text.splitlines(), 1)
		)
	_PATHS, _LINES = tuple(paths), tuple(lines)


def _find(query: str, root: str, limit: int) -> FindResult:
	if _PATHS is None:
		raise RuntimeError("fuzzy index worker boot did not complete")
	query_grams = trigrams(query)
	scored = (
		FileMatch(
			record.text,
			fuzzy_score(
				query,
				record.text,
				query_grams=query_grams,
				candidate_grams=record.grams,
			),
		)
		for record in _PATHS
		if under_root(record.text, root)
	)
	matches = tuple(
		sorted(
			(match for match in scored if match.score > 0),
			key=lambda match: (-match.score, match.path),
		)[:limit]
	)
	return FindResult(query, len(_PATHS), matches)


def _grep(query: str, root: str, limit: int, case: bool) -> GrepResult:
	if _PATHS is None or _LINES is None:
		raise RuntimeError("fuzzy index worker boot did not complete")
	query_grams = trigrams(query, case_sensitive=case)
	scored: list[ContentMatch] = []
	for record in _LINES:
		if not under_root(record.path, root):
			continue
		score = fuzzy_score(
			query,
			record.text,
			query_grams=query_grams,
			candidate_grams=record.exact_grams if case else record.grams,
			case_sensitive=case,
		)
		if score > 0:
			scored.append(ContentMatch(record.path, record.number, record.text, score))
	matches = tuple(
		sorted(scored, key=lambda match: (-match.score, match.path, match.line))[:limit]
	)
	return GrepResult(query, len(_PATHS), matches)


def _spill_if_needed(result: FindResult | GrepResult) -> FindResult | GrepResult | omp.Spill:
	encoded = _json.dumps(_asdict(result), ensure_ascii=False, separators=(",", ":")).encode()
	if len(encoded) > omp.workers.RESULT_SPILL_BYTES:
		return omp.Spill(encoded)
	return result


omp.workers.declare(
	omp.WorkerSpec(
		name="fuzzy-index",
		site=omp.Site.ENV,
		boot=_build_index,
		warm=True,
		idle_ttl=omp.Duration("0s"),
		max_concurrency=8,
		restart=omp.Restart.ON_FAILURE,
		resources=omp.WorkerResources(memory_bytes=1 << 30, cpu_shares=2.0),
	)
)


@omp.device("ffind", family="fff", rev=1, place="worker:fuzzy-index")
async def ffind(args: FindArgs, ctx: omp.Context) -> FindResult | omp.Spill:
	"""Return fuzzy-ranked paths from the worker's warm workspace index."""

	del ctx
	return _spill_if_needed(_find(args.query, args.root, bounded_limit(args.limit)))


@omp.device("fgrep", family="fff", rev=1, place="worker:fuzzy-index")
async def fgrep(args: GrepArgs, ctx: omp.Context) -> GrepResult | omp.Spill:
	"""Return fuzzy-ranked lines from the worker's warm workspace index."""

	del ctx
	return _spill_if_needed(
		_grep(args.query, args.root, bounded_limit(args.limit), args.case)
	)
