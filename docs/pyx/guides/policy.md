# Policy

Use policy hooks and sandbox profiles when you need to inspect an operation, deny it, request approval, or describe its confinement. A hook observes or edits one inference, turn, or call and cannot own a yield. Behavior that must keep control of the loop across turns is a Director, and durable state derived from the journal is a Component; both are declared with [`omp.extensions`](../reference/omp.extensions.md). The systems complement each other: Directors shape loop behavior, while policy decides which effects may proceed.

```python
import omp


@omp.hook("tool_call", phase=omp.HookPhase.PRECHECK)
def deny_network(event: omp.ToolCallEvent, ctx: omp.Context) -> omp.HookDecision:
    if event.bash is not None and event.bash.net:
        return omp.Deny("network effects are disabled", code="network.denied")
    return omp.Defer()
```

The policy hook refuses shell calls whose host-analyzed [`BashIR`](../reference/omp.policy.md#omppolicybashir) contains a network reference.

## Keep control across turns

A Director holds exclusive resources by claiming a slot (`mode`, `loop`, `tool_choice`, or `worktree`), refines the next inference, and judges each candidate yield with one verdict: `pass`, `continue`, `yield`, `push`, `done`, or `fail`. Its durable state is a set of scalar properties on its own element in the session tree, committed through `updates`; module globals are never authoritative.

```python
@omp.director("bounded-continuations", claims=("loop",))
class BoundedContinuations:
    LIMIT = 3

    def on_yield(self, event):
        used = int(event["state"].get("used", 0))
        if used >= self.LIMIT:
            return "done"
        return {"verdict": "continue", "updates": {"used": used + 1}}
```

Callbacks run in the killable extension host and cannot mutate live agent state; their effects are the returned verdict, `updates`, and DOM `ops`. See [`omp.extensions`](../reference/omp.extensions.md) for the full contract.

## Deny effects with policy hooks

A policy hook sees a logical call before the environment authorizes its effects. For shell execution, `event.bash` is a [`BashIR`](../reference/omp.policy.md#omppolicybashir) produced by the host analyzer. Prefer structured facts over matching the source string:

- `ir.reads` and `ir.writes` contain filesystem effects.
- `ir.net` contains all inferred network references; `ir.net_sinks()` narrows these to egress and bidirectional references.
- `ir.has_dynamic_eval` marks execution the analyzer cannot fully determine.
- `ir.is_read_only()` requires no writes, no network, no dynamic evaluation, and read-only classification for every command.

Return `omp.Deny(reason, code=...)` from the appropriate hook phase to refuse a call. A denial is distinct from a sandbox violation: admission prevents the invocation from starting, while a violation reports an attempted effect against installed confinement.

For non-shell path arguments, use [`await omp.policy.match_paths()`](../reference/omp.policy.md#omppolicymatch_paths) so resolution occurs in the environment that owns the path. Do not use host-side `os.path` calls for remote workspace paths.

## Request approval

Approval is a durable Core-owned ticket, not a suspended extension coroutine:

1. An approval-phase hook returns `omp.RequireApproval(omp.ApprovalSpec(...))`.
2. Core aggregates unresolved reasons for the invocation into one `ApprovalTicket`.
3. The invocation parks while other calls continue.
4. A user, external approver, configuration rule, timeout, or unavailable-route rule produces an `ApprovalDecision`.
5. Core resumes an approved invocation or records a structured policy denial.

`APPROVAL_DEADLINE` is `Duration("5m")` and is the default timeout used by `@omp.approver`. External approvers must be async and idempotent by `ticket.ticket_id`, because a pending ticket may be offered again after restart.

```python
@omp.approver("operations", kinds=(omp.ApprovalKind.NETWORK,))
async def operations(ticket: omp.ApprovalTicket, ctx: omp.Context):
    approved = await ask_operations_service(ticket)
    return omp.ApprovalDecision(
        approved=approved,
        scope=omp.PolicyScope.ONCE,
        source=omp.ApprovalSource.EXTERNAL,
        decided_by="operations",
        reason=None,
        audited=True,
    )
```

Use [`pending()`](../reference/omp.policy.md#omppolicypending) to reconcile outstanding tickets and [`decide()`](../reference/omp.policy.md#omppolicydecide) to submit a decision. An identical repeated decision is an idempotent no-op; a conflicting decision is rejected by Core.

## Profiles, budgets, and quotas

A [`SandboxProfile`](../reference/omp.policy.md#omppolicysandboxprofile) groups filesystem, network, executable, and process-resource policy. Profiles are immutable data. [`install()`](../reference/omp.policy.md#omppolicyinstall) installs a scoped contribution that may only narrow running confinement; [`ProfileHandle.revoke()`](../reference/omp.policy.md#omppolicyprofilehandle) removes that contribution.

`ResourceBudget` provides per-process ceilings for wall time, CPU, memory, output, child count, and disk/file usage. The host-wide constants in [`omp.limits`](../reference/omp.limits.md) set separate protocol and runtime ceilings, including frame size, child count, pending effects, reentrancy, observation capacity, and shutdown timing.

These limits serve different layers:

- A Director's verdicts bound loop continuation.
- `ResourceBudget` bounds a confined execution session.
- `omp.limits` describes fixed host ceilings and compatibility revisions.

Do not treat a quota failure as a policy denial. Resource exhaustion is a failed execution or a `ViolationKind.RESOURCE`; a denial means policy refused authorization.

## Related reference

- [`omp.extensions`](../reference/omp.extensions.md)
- [`omp.policy`](../reference/omp.policy.md)
- [`omp.limits`](../reference/omp.limits.md)
- [Hooks guide](hooks.md)
- [Environment guide](environment.md)
