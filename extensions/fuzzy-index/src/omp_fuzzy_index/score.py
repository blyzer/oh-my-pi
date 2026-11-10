"""Pure fuzzy trigram scoring helpers (no live omp host)."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class FileMatch:
	"""Report one ranked workspace-path match."""

	path: str
	score: float


@dataclass(frozen=True, slots=True)
class ContentMatch:
	"""Report one ranked workspace-content match."""

	path: str
	line: int
	text: str
	score: float


def trigrams(value: str, *, case_sensitive: bool = False) -> frozenset[str]:
	normalized = value if case_sensitive else value.casefold()
	if len(normalized) < 3:
		return frozenset((normalized,)) if normalized else frozenset()
	return frozenset(
		normalized[index : index + 3] for index in range(len(normalized) - 2)
	)


def fuzzy_score(
	query: str,
	candidate: str,
	*,
	query_grams: frozenset[str],
	candidate_grams: frozenset[str],
	case_sensitive: bool = False,
) -> float:
	needle = query if case_sensitive else query.casefold()
	haystack = candidate if case_sensitive else candidate.casefold()
	if not needle or not haystack:
		return 0.0

	exact = haystack.find(needle)
	exact_score = 3.0 + len(needle) / len(haystack) if exact >= 0 else 0.0

	positions: list[int] = []
	cursor = 0
	for character in needle:
		found = haystack.find(character, cursor)
		if found < 0:
			positions = []
			break
		positions.append(found)
		cursor = found + 1
	subsequence_score = 0.0
	if positions:
		span = positions[-1] - positions[0] + 1
		subsequence_score = 1.0 + len(needle) / span + 1.0 / (1 + positions[0])

	overlap_score = 0.0
	if query_grams:
		overlap_score = len(query_grams & candidate_grams) / len(query_grams)
	return max(exact_score, subsequence_score + overlap_score)


def bounded_limit(limit: int) -> int:
	if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 500:
		raise ValueError("limit must be an integer from 1 through 500")
	return limit


def under_root(path: str, root: str) -> bool:
	normalized = root.strip().strip("/")
	return not normalized or normalized == "." or path.strip("/").startswith(normalized + "/")


__all__ = (
	"ContentMatch",
	"FileMatch",
	"bounded_limit",
	"fuzzy_score",
	"trigrams",
	"under_root",
)
