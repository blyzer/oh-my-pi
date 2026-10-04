# 0013. Children seed from the parent; cfg files, not per-setting inherit flags

Status: accepted
Date: 2026-09-02
Area: control-plane

## Context

In omp v1 the service tier (`/fast`) needed a second setting just to decide what a subagent gets:

```yaml
tier:
  openai: priority
  subagent: inherit   # separate setting
```

Every setting that a child might see was on course to grow the same `subagent:` sub-key, each
with its own default, its own resolution order, and its own place on the session god object that
0012 removes. The inheritance rule for a value lived nowhere near the value's definition, and a
user who wanted "children pinned to cheap" had to discover the right per-setting escape hatch.

Source Engine games never had this problem: a client's variables start from the server's live
values, and `config.cfg`, class cfgs, and user cfgs are plain files of console commands executed
at well-known moments. TF2 shipped per-class configs (`scout.cfg`, `medic.cfg`) auto-executed on
class change; users layered their own profiles on top without any schema for doing so.

## Decision

A value that must survive the session and reach children is ONE variable, flagged `SESSION`
(0012): it is journaled with the session, so resuming restores it.

Inheritance is NOT a flag. A spawned child MUST seed every variable from the parent's live values
at spawn time, by default. There is nothing to opt into and no per-setting `inherit` key.

Overrides are expressed as cfg files — ordinary command-stream scripts (0014) executed at fixed
points:

- `config.cfg` — executed for the main session;
- user cfgs — any number, executed on demand as profiles;
- `subagent.cfg` — auto-executed for every spawn, after seeding;
- `<agent>.cfg` — auto-executed when an agent of that class spawns, layered on top of
  `subagent.cfg`.

```sh
# subagent.cfg — auto-exec'd for every spawn
ai_fastmode 0

# sonic.cfg — auto-exec'd when a sonic spawns, class config
ai_model @smol
ai_thinking low
```

Precedence is the execution order: parent live values, then `subagent.cfg`, then `<agent>.cfg`,
then whatever the spawner sets explicitly. No setting MAY carry a private inheritance rule that
bypasses this order.

## Consequences

- One value describes the main session and its children. Pinning children is a one-line cfg
  entry, and class behavior (`sonic` on the small model with low thinking) is a file a user can
  read and edit without learning a schema.
- The session object loses a family of `*.subagent` properties; the inheritance rule lives where
  the value is defined or in a cfg the user owns.
- Resume restores a child's values from its own journal, because the seeded values were written
  into the child's session node at spawn (0003, 0004).
- Prohibited: `subagent:`/`inherit` sub-keys, agent-class-specific fields on the settings
  document, and code paths that compute a child's value by consulting the parent at read time
  rather than seeding at spawn.
- Cost accepted: seeding copies every session-flagged variable per spawn. The variable set is
  small and the copy is one journal write per spawn; that is cheaper than the read-time
  resolution chains it replaces.

## Status in omp

**Status: Implemented.** Children seed from the parent and then run `subagent.cfg` and `<agent>.cfg`, with no per-setting inherit flag. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Seeding and cfg execution: `crates/driver/src/subagent/settings.rs`, `Ctx::exec_spawn_configs` and `Ctx::exec_configs` in `crates/con/src/ctx.rs`; layers `Origin::Inherited` and `Origin::Class` in `crates/con/src/layers.rs`.
- Resume of a child re-applies its class through `subagent::spawn::configure_resumed_child` (adopted by `headless::con_journal`).
- User cfg location is `~/.o2` (`omp_core::dirs::config_dir`), an owner decision recorded in `AGENTS.md`.

### Implementation notes (carried over)

Primary implementation: `crates/driver/src/subagent/settings.rs`. Child contexts seed effective values and execute `subagent.cfg` plus class cfg. User cfg files live in `~/.o2` (`omp_core::dirs::config_dir`, `OMP_CONFIG_DIR` override; owner decision 2026-09-03); `<project>/.omp/config.cfg` overlays. Cfg execution is lenient (`Ctx::exec_configs` reports and skips unknown names). `ai_task_model` is seeded as the child's `ai_model` ahead of both cfgs, so a class cfg's own `ai_model` outranks it; a class cfg's `ai_model inherit` runs the child on the parent's own `ai_model` instead, and an `ai_model` the cfgs leave empty (`reset ai_model`) falls back to the seed (`ai_task_model`, else the parent's). A `sv_task_agent_model_overrides` entry is the spawner's explicit choice and is applied after them. The seed lands in the child's inherited layer and the cfgs plus the spawner's route in its class layer (`omp_con::Origin`), beneath the child's own session writes; `reset <var>` removes a layer's value rather than writing the inherited one, so a child's console `reset` returns to its class value, else the parent's. The main chat resuming a child session (`/resume`, `--resume`) re-applies the child's class through the same spawn path (`subagent::spawn::configure_resumed_child`, adopted by `headless::con_journal`) at the recursion depth the child journals beside its class, beneath the child's journaled writes, and drops it on switching back; a class whose `<agent>.cfg` is gone resumes on the default subagent configuration with a console notice. While a child is presented the main scope's session layer is parked (`Ctx::adopt_scope`): the child's picture is its own journaled writes, then its class, then its inherited seed — the main scope's picture (`Ctx::scope_seed`: its session writes over the archive, never engagement binds or the child's own writes) — then the archive and defaults. The main session's writes therefore never outrank the child's class nor are journaled into it, and `Ctx::drop_scope` restores them on switching back. The kernel's `task` tool follows the presented session as well (`SessionTool::advertised`): a resumed child at the recursion ceiling is not advertised `task`, as a child spawned at that depth is not; the roster changes only at that session boundary.

## References

- The Harness Playbook, "The control plane" → "Inheritance should not require a second setting"
- Team Fortress 2 class cfg files (`<class>.cfg` auto-exec); Source `config.cfg`
- 0012 (convars), 0014 (command stream and cfg execution), 0003, 0004
- `crates/driver/src/headless/kernel.rs` (`compose_kernel`), `crates/driver/src/cfg.rs`,
  `crates/con/src/layers.rs` (spawn seeds), `crates/driver/src/subagent/settings.rs`. The former
  `crates/driver/src/chat.rs`, `crates/driver/src/subagent/snapshot.rs`, and
  `crates/agent/src/tree.rs` no longer exist; the tree is `crates/dom` and `crates/session`
