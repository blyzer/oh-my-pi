"""Pure Brave search response parsing (no live omp host)."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class SearchResult:
	"""Represent one normalized ranked web result."""

	title: str
	url: str
	snippet: str
	rank: int


def parse_results(payload: object, *, rank_offset: int = 0) -> list[SearchResult]:
	if not isinstance(payload, Mapping):
		raise ValueError("search response must be an object")
	web = payload.get("web", payload)
	if not isinstance(web, Mapping):
		raise ValueError("search response web field must be an object")
	raw_results = web.get("results")
	if not isinstance(raw_results, Sequence) or isinstance(raw_results, (str, bytes)):
		raise ValueError("search response results field must be a list")
	results: list[SearchResult] = []
	for raw in raw_results:
		if not isinstance(raw, Mapping):
			raise ValueError("each search result must be an object")
		title, url = raw.get("title"), raw.get("url")
		snippet = raw.get("description", raw.get("snippet", ""))
		if not isinstance(title, str) or not isinstance(url, str) or not isinstance(snippet, str):
			raise ValueError("search result title, url, and snippet must be strings")
		results.append(SearchResult(title, url, snippet, rank_offset + len(results) + 1))
	return results


__all__ = ("SearchResult", "parse_results")
