"""Host approver/hooks for ``omp.remote-approve`` (needs frozen omp at import)."""

from __future__ import annotations

import json
from urllib.parse import urlencode

import omp

from .decisions import (
	RemoteReply,
	decode_reply,
	parse_unreachable,
	remote_decision as _pure_remote_decision,
	unreachable_decision as _pure_unreachable,
)

_POLL_PROMPT = "[remote-approve:poll]"


class RemoteUnavailable(RuntimeError):
	"""Report that the configured approval service could not answer."""


def _webhook(ctx: omp.Context) -> str:
	value = str(ctx.settings.get("webhook", "")).strip()
	if not value.startswith(("http://", "https://")):
		raise RemoteUnavailable("settings.webhook must be an absolute HTTP URL")
	return value


def _query_url(ctx: omp.Context, action: str, **values: str) -> str:
	base = _webhook(ctx)
	separator = "&" if "?" in base else "?"
	return f"{base}{separator}{urlencode({'action': action, **values})}"


async def _request(url: str) -> omp.env.HttpResponse:
	return await omp.env.http_get(url, timeout=omp.Duration("10s"))


def _decode(response: omp.env.HttpResponse) -> RemoteReply:
	try:
		body = response.json()
	except Exception:
		body = None
	try:
		return decode_reply(response.status, body)
	except ValueError as error:
		raise RemoteUnavailable(str(error)) from error


def _ticket_payload(ticket: omp.ApprovalTicket) -> str:
	return json.dumps(
		{
			"ticket_id": ticket.ticket_id,
			"invocation_id": ticket.invocation_id,
			"created_at": ticket.created_at,
			"reasons": [
				{
					"title": reason.title,
					"body": reason.body,
					"subject": reason.subject,
					"kind": reason.kind.value,
					"scopes": [scope.value for scope in reason.scopes],
					"evidence": list(reason.evidence),
				}
				for reason in ticket.reasons
			],
		},
		separators=(",", ":"),
		sort_keys=True,
	)


def _to_host_decision(
	ticket: omp.ApprovalTicket, local: object
) -> omp.ApprovalDecision | None:
	from .decisions import LocalDecision

	if local is None or not isinstance(local, LocalDecision):
		return None
	source = (
		omp.ApprovalSource.EXTERNAL
		if local.source == "external"
		else omp.ApprovalSource.UNAVAILABLE
	)
	del ticket
	return omp.ApprovalDecision(
		approved=local.approved,
		scope=omp.PolicyScope.ONCE,
		source=source,
		decided_by="remote-approve",
		reason=local.reason,
		audited=local.audited,
	)


async def remote_status(ticket_id: str, ctx: omp.Context) -> RemoteReply:
	"""Read the remote service's durable state for one Core ticket."""

	try:
		response = await _request(_query_url(ctx, "status", ticket_id=ticket_id))
	except Exception as error:
		if isinstance(error, RemoteUnavailable):
			raise
		raise RemoteUnavailable("approval service is unreachable") from error
	return _decode(response)


async def forward_once(
	ticket: omp.ApprovalTicket, ctx: omp.Context
) -> RemoteReply:
	"""Offer a ticket only when its remote idempotency key is absent."""

	status = await remote_status(ticket.ticket_id, ctx)
	if status.state != "missing":
		return status
	try:
		response = await _request(
			_query_url(
				ctx,
				"offer",
				ticket_id=ticket.ticket_id,
				payload=_ticket_payload(ticket),
			)
		)
	except Exception as error:
		if isinstance(error, RemoteUnavailable):
			raise
		raise RemoteUnavailable("approval service is unreachable") from error
	offered = _decode(response)
	if offered.state == "missing":
		raise RemoteUnavailable("approval service did not retain the offered ticket")
	return offered


def remote_decision(
	ticket: omp.ApprovalTicket, reply: RemoteReply
) -> omp.ApprovalDecision | None:
	"""Translate a completed remote answer without inventing a pending answer."""

	return _to_host_decision(ticket, _pure_remote_decision(reply))


def unreachable_decision(
	ticket: omp.ApprovalTicket,
	mode: str,
	*,
	allow_fail_open: bool,
) -> omp.ApprovalDecision | None:
	"""Map an unavailable service to local fallback, denial, or gated read-only allow."""

	all_read = all(reason.kind is omp.ApprovalKind.READ for reason in ticket.reasons)
	return _to_host_decision(
		ticket,
		_pure_unreachable(
			parse_unreachable(mode),
			allow_fail_open=allow_fail_open,
			all_read=all_read,
		),
	)


async def _fallback(
	ticket: omp.ApprovalTicket, ctx: omp.Context
) -> omp.ApprovalDecision | None:
	return unreachable_decision(
		ticket,
		str(ctx.settings.get("unreachable", "ask")),
		allow_fail_open=ctx.settings.get("allow_fail_open") is True,
	)


@omp.approver(
	"remote-approve",
	kinds=tuple(omp.ApprovalKind),
	timeout=omp.Duration("5m"),
	unreachable=omp.Unreachable.ESCALATE_LOCAL,
)
async def remote_approver(
	ticket: omp.ApprovalTicket, ctx: omp.Context
) -> omp.ApprovalDecision | None:
	"""Idempotently offer a Core-owned ticket without creating or awaiting one."""

	try:
		reply = await forward_once(ticket, ctx)
	except RemoteUnavailable:
		return await _fallback(ticket, ctx)
	return remote_decision(ticket, reply)


async def reconcile_pending(ctx: omp.Context) -> None:
	"""Poll remote answers and resolve only still-pending Core tickets."""

	for ticket in await omp.policy.pending():
		try:
			reply = await remote_status(ticket.ticket_id, ctx)
			decision = remote_decision(ticket, reply)
			if reply.state == "missing":
				decision = remote_decision(ticket, await forward_once(ticket, ctx))
		except RemoteUnavailable:
			decision = await _fallback(ticket, ctx)
		if decision is not None:
			await omp.policy.decide(ticket.ticket_id, decision)


@omp.hook("extension_activate", phase=omp.HookPhase.OBSERVE)
async def activate_remote_approval(
	event: omp.ExtensionActivateEvent, ctx: omp.Context
) -> None:
	"""Upsert polling and reconcile tickets re-offered after activation."""

	del event
	interval = omp.Duration(str(ctx.settings.get("poll_interval", "30s")))
	await omp.agents.schedule(
		"remote-approve-poll",
		omp.agents.Every(interval),
		omp.agents.Inject(
			_POLL_PROMPT,
			mode=omp.agents.DeliveryMode.NEXT_TURN,
			visible=False,
		),
		scope=omp.agents.ScheduleScope.SESSION,
		missed=omp.agents.MissedRunPolicy.COALESCE,
		overlap="skip",
	)
	await reconcile_pending(ctx)


@omp.hook("turn_start", phase=omp.HookPhase.OBSERVE)
async def poll_remote_approval(
	event: omp.TurnStartEvent, ctx: omp.Context
) -> None:
	"""Poll pending tickets when an invisible Every firing wakes the agent."""

	del event
	await reconcile_pending(ctx)
