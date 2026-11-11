"""Host provider/device for ``omp.search-provider`` (needs frozen omp at import)."""

from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

import omp
from omp import (
	Api,
	AuthMode,
	AuthSpec,
	CredentialSource,
	ModelSpec,
	Operation,
	Payload,
	ProviderSpec,
	RouteSpec,
)

from .parse import SearchResult, parse_results

_AUTH = AuthSpec(
	mode=AuthMode.API_KEY,
	header="x-subscription-token",
	prefix="",
	sources=(CredentialSource.stored(), CredentialSource.env("BRAVE_SEARCH_API_KEY")),
)
_PROVIDER = ProviderSpec(
	id="brave-search",
	name="Brave Search",
	routes=(
		RouteSpec(
			id="web",
			base_url="https://api.search.brave.com/res/v1/web/search",
			api=Api.SEARCH_HTTP,
			auth=_AUTH,
			headers={"accept": "application/json"},
		),
	),
	models=(
		ModelSpec(
			id="web",
			display_name="Brave Web Search",
			routes=("web",),
			operations=frozenset({Operation.SEARCH}),
		),
	),
)


@omp.provider(_PROVIDER)
class BraveSearch:
	"""Declare the Brave SEARCH endpoint and brokered API-key placement."""


@dataclass(frozen=True, slots=True)
class SearchArgs:
	"""Describe one page of a web search."""

	query: str
	page: int = 1
	page_size: int = 10


@dataclass(frozen=True, slots=True)
class SearchResults(Payload):
	"""Return one requested page of normalized search results."""

	query: str
	page: int
	results: list[SearchResult]


@dataclass(frozen=True, slots=True)
class SearchFault(omp.Fault):
	"""Report a typed search transport or response failure."""

	kind: str
	detail: str
	status: int | None = None


def _search_sync(args: SearchArgs, api_key: bytes) -> SearchResults | SearchFault:
	page = max(1, args.page)
	page_size = min(20, max(1, args.page_size))
	offset = (page - 1) * page_size
	query = urlencode({"q": args.query, "count": page_size, "offset": offset})
	request = Request(
		f"https://api.search.brave.com/res/v1/web/search?{query}",
		headers={
			"accept": "application/json",
			"x-subscription-token": api_key.decode("utf-8"),
			"user-agent": "omp-search-provider/0.1",
		},
	)
	try:
		with urlopen(request, timeout=20) as response:
			payload = json.load(response)
	except HTTPError as error:
		return SearchFault("http", str(error.reason), error.code)
	except (URLError, OSError, ValueError, UnicodeError, json.JSONDecodeError) as error:
		return SearchFault("network", str(error))
	try:
		return SearchResults(args.query, page, parse_results(payload, rank_offset=offset))
	except ValueError as error:
		return SearchFault("response", str(error))


@omp.device("web_search", family="ws", rev=1, place="env")
async def web_search(args: SearchArgs, ctx: omp.Context) -> SearchResults | SearchFault:
	"""Search through the declared backend and return the requested ranked page."""

	credential_id = str(ctx.settings.get("credential_id", "")) or None
	# credential_id may be empty; host reveal accepts provider lookup alone.
	reveal_id = int(credential_id) if credential_id and credential_id.isdigit() else None
	secret = await omp.creds.reveal(id=reveal_id, provider="brave-search")
	with secret.use() as api_key:
		return await asyncio.to_thread(_search_sync, args, bytes(api_key))
