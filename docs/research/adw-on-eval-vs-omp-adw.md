# ADW on eval versus `omp adw`: one task, two shapes

Status: research note with one real `omp adw` run and **no successful eval-script run**.
Date: 2026-10-13
Base: `origin/omp2` at `734625e5b7`. Binary used: `target/debug/omp` (`omp/2.0.0`, built 2026-10-12 10:50, not rebuilt).
Artifacts: `examples/adw-on-eval/{adw_on_eval.py,greet-files.toml,README.md}`.

Every claim cites `file:line` in this worktree, or says what was observed. **UNVERIFIED** marks anything I did not run.

## Summary

- The eval script `adw_on_eval.py` **did not run**, and nothing in this note shows it works. In the built binary every eval cell is refused
  with `eval bridge parent session is not bound for this owner` (section 4). The source shows a likely cause, which I did not confirm by patching.
- `omp adw run greet-files` did run and was accepted (section 3). Along the way it exposed three defects in `omp adw` itself (section 5).
- The two shapes are not substitutes. `omp adw` is serial, posture-checked and sandboxed. Eval fans out in parallel with isolated worktrees but has no posture check, no sandboxed gate and no resume.

## 1. The task, in both shapes

Write `greet.py` and `farewell.py`, each starting `# <file>: <purpose>`, each followed by a deterministic gate on that first line.

| | `omp adw` | eval script |
|---|---|---|
| Declared in | `.omp/workflows/greet-files.toml` (5 phases: `plan`, `write_greet`, `gate_greet`, `write_farewell`, `gate_farewell`) | `adw_on_eval.py`, one Python cell |
| Plan | an `agent` phase that writes `PLAN.md` | one workpool item that yields `{"files": [...]}`, validated against a fixed file set |
| Writers | two serial `agent` phases | one pool item per file, in parallel |
| Gate | a `code` phase: `python3 -c ...` with the file as argv | `gate()` run in the kernel |
| Retry | `on_reject = "correct"` rewinds to the writer with the gate's stdout as correction | the next round's pool item carries the gate's feedback; at most 3 attempts per file |

`agent()`, `completion()` and `judge` are not used in the script. The production parent host reports them unavailable and binds only `__workpool__`
(`crates/driver/src/subagent/workpool_scheduler.rs:812-885` `WorkpoolSessionHost`, `:943-945`; `docs/adr/STATUS.md:66`), so even the plan step is a pool item.

### What already existed (sibling `/private/tmp/omp-next-adw-eval`)

Nothing to build on. Branch `feat/adw-on-eval-example` is at `7122178f0e`, an ancestor of this worktree's base. It has zero commits over `origin/omp2` and `examples/` holds only the placeholder `README.md`.
The only prior art is an untracked scratch demo, `/private/tmp/adw-eval-demo/adw_on_eval.py` plus `dyn-eval-fix.md`. Neither is in git.
That demo uses `agent(...).wait()`, `tool.bash` and `judge`. The prelude's `agent()` returns the result directly rather than a handle (`crates/envd/src/eval/python_prelude.py:841-916`),
and `judge` appears nowhere in the omp2 prelude (grep of `python_prelude.py`). This example differs by using only the pool, and by comparing against a real `omp adw` run.
`dyn-eval-fix.md` found the same family of failure that section 4 reports (an eval cell with no authenticated owner). That led to `25bb0fda35`, merged as `#283` (`89ed516c13`), which is not in the binary I used (section 4).

## 2. What each one guarantees

### Posture (`Requirement`) checks

- **`omp adw`:** a phase may declare `requires = { write, network, approval }` (kebab-case, `crates/adw/src/profile.rs:33,59,85,105-112`).
  `Run::next_step` checks it before dispatch and returns `Step::Blocked` when the host posture is looser (`crates/adw/src/run.rs:249-260`; `Requirement::check` at `crates/adw/src/profile.rs:231`).
  An unreported posture also blocks (`run.rs:252-254`). A stricter host passes (`profile.rs:222-224`).
  The host reports the sandbox it really has: `production.rs:331-368` degrades `write` to `Unconfined` when a requested sandbox did not construct.
  **Observed:** `needs-offline.toml` (`requires = { network = "disabled" }`) ran as
  `Workflow `needs-offline` blocked: resolved network scope is looser than the phase requires.`, exit 1, with nothing dispatched.
  `docs/adr/0028-bash-is-an-in-process-interpreter.md:171-174` says the default is `scoped`, so this is the designed outcome.
- **Eval:** none. A cell declares no requirement and nothing compares one to a posture. The cell itself compiles with network `disabled` (`docs/adr/0028-bash-is-an-in-process-interpreter.md:168`, not run here).
  Pool workers are child kernels under the session's policy.
- **Limit of the adw check:** the posture is resolved once per run (`mod.rs:132-135`), and the `Requirement` is only compared, never imposed. A phase asking for `read-only` is blocked on a looser host; it is not run inside a read-only child.

### Containment

- **`omp adw` code phases** run through the project Environment's exec authority so the reported sandbox confines them (`production.rs:1-6,177-226`; proven by the test at `production.rs:686-741`).
  They run only in a trusted workspace; otherwise `ProductionError::UntrustedWorkspace` (`production.rs:380-383`). The definition loader refuses an executable given by path or a `cwd` that escapes the project (`definition.rs:182-191`),
  and `deny_unknown_fields` closes the schema (`definition.rs:222-250`). `docs/adr/0040-remote-build-and-test-execution.md:154-160` and `docs/adr/0047-agent-build-execution-model.md:12` still call code phases unconfined; they predate `9dd09d888f`.
- **`omp adw` model phases** run as ephemeral child kernels without workspace extensions (`production.rs:229-245`) **in the real project root** (`production.rs:159-165`), with no isolation.
  **Observed:** the `plan` phase wrote `PLAN.md` into the project. In an untrusted workspace that happened *before* the first code phase was refused, so the run failed after already changing files.
- **Eval pool workers** each get an isolated worktree (`workpool_runtime.rs:396`) and merge it back with `merge_worktree` on retirement, applying by default (`spawn.rs:1006-1043`, `settings.rs:374-377`). UNVERIFIED end to end.
- **Eval gate** is plain Python in the kernel (`read(path)`), with the kernel's authority. There is no argv, no exec sandbox and no trust gate on what the script itself does.

### Retries

- **`omp adw`:** three routes per phase: `retry`, `correct`, `halt` (`crates/adw/src/workflow.rs:38-51`). The rejection text is stored as the next attempt's correction (`run.rs:348,359`) and shown in the child's brief (`production.rs:425-428`).
  `correct` rewinds to the nearest upstream producer, invalidates its dependents (`run.rs:366-368`), and refuses to accept an attempt built on a superseded producer version (`run.rs:299-315`).
  **Defect:** `correct` has no attempt ceiling. Only `retry` is checked against `max_attempts` (`run.rs:341-346`), and every rewind grants the producer one more (`run.rs:363`).
  **Observed** (an earlier gate version): `write_greet` ran **8 times** and `gate_greet` rejected 7 times under the default `--max-attempts 3`; see section 3.
- **Eval:** whatever the script codes. This one caps at `MAX_ATTEMPTS = 3` per file and halts the file, not the others. That is a policy I chose, not a guarantee of the platform.

### Journaling and resume

- **`omp adw`:** nothing is journaled and there is no resume. `omp adw run` prints the records at the end and exits 0/1/130 (`adw_cmd.rs:80-109`). `grep journal` over `crates/driver/src/adw/*.rs` finds nothing.
  `docs/adr/0039-remote-control-and-factory-modes.md:37` and `docs/adr/0001-design-envelope.md:59` say the same.
  Worse, a host failure aborts the whole run with no record: `dispatch(...).await.map_err(Error::Host)?` (`mod.rs:160-163`).
- **Eval:** the pool's items, workers and batches are persisted into the session journal (`workpool_scheduler.rs:992-1082`, `WORKPOOL_STATE`). The script's own state (`attempts`, `pending`) lives in kernel globals and dies with the kernel.
  No code resumes a half-done script. UNVERIFIED: whether a `--resume`d session could re-attach a pool by name.

### Cancellation

- **`omp adw`:** Ctrl-C cancels a token (`adw_cmd.rs:62-78`). The loop checks it before each step and after each dispatch, so a cut-short phase is not scored as a rejection (`mod.rs:143,164-169`, test `mod.rs:295-320`).
  A code phase's process tree is torn down TERM then KILL (`production.rs:278-285`) and a child turn takes the token through `RunControl` (`production.rs:167-172`). Not exercised here.
  `docs/adr/0040-remote-build-and-test-execution.md:160` ("Its cancel token is never triggered") is out of date.
- **Eval:** the script has no signal path. `pool.close()` in a `finally` drops queued items but "lets active turns settle" (`workpool_scheduler.rs:499`), so it does not stop a running worker.
  `Workpool::cancel` and `release_owner` exist (`:518,682`) but the Python `WorkPool` exposes neither (`python_prelude.py:919-957`). UNVERIFIED how a cell interrupt unwinds a script mid-poll.

## 3. What I ran

All in a throwaway git repo, models limited to `omniroute/cohere/command-a-plus-05-2026` and `omniroute/cohere/command-r7b-12-2024`.

**`omp adw list`:** `greet-files (5 phases)` with `plan [agent]`, `write_greet [agent]`, `gate_greet [code]`, `write_farewell [agent]`, `gate_farewell [code]`; and `needs-offline (1 phases)`.

**Untrusted workspace** (the default): `× workflow runtime host failed ╰─▶ workspace is not trusted; adw code phases are withheld`, after `plan` had already written `PLAN.md`.

**Trusted workspace, final workflow file**, `command-a-plus`, exit 0, 1m12.9s:

```
plan attempt 1: accepted — accepted as v1
write_greet attempt 1: accepted — accepted as v1
gate_greet attempt 1: accepted — accepted as v1
write_farewell attempt 1: accepted — accepted as v1
gate_farewell attempt 1: accepted — accepted as v1
Workflow `greet-files` accepted every phase.
```

The files: `# greet.py: Simple greeting function` and `# farewell.py: farewell function for workflow completion`, each followed by the requested function. A previous run, with a looser first-draft gate that only checked `startswith("# <file>: ")`, produced a rewind loop; its output was:

```
write_greet attempt 1..8: accepted — accepted as v1..v8
gate_greet attempt 1..7: rejected — rewound to `write_greet`
gate_greet attempt 8: accepted — accepted as v1
write_farewell attempt 1: accepted — accepted as v1
gate_farewell attempt 1: rejected — rewound to `write_farewell`
write_farewell attempt 2: accepted — accepted as v2
gate_farewell attempt 2: accepted — accepted as v1
Workflow `greet-files` accepted every phase.     (5m18.9s)
```

I did not keep the rejection text for those attempts, so I cannot say why the first draft gate rejected `greet.py` seven times. That run proves only the rewind and version bookkeeping, and the missing ceiling.

**Failed runs.** Six attempts aborted the whole run with a provider or configuration error, none of which `omp adw` retried or recorded:

- `command-r7b`, three times: `inference CapabilityUnknown error during Planning: capability chat.tools.choice unavailable (catalog-capability-evidence)`. Once it got past `plan` first.
- `command-a-plus`, once after 4m38s with both files already written: `inference EmptyOutput error during Recovery: protocol violation (empty-completion.thought-only)`.
- twice with a user `ai_model` cfg line set despite `-m`: `Ambiguous model 'glm-4.7-flash'` (section 5).

## 4. Why the eval script did not run

I tried four ways to execute any eval cell, all against `target/debug/omp`, plus the second prebuilt binary below:

| Attempt | Result |
|---|---|
| `omp print --py-eval` driving `dyn eval py -` through `bash` | `dyn: dynamic target `eval` is denied by approval policy` (`crates/envd/src/admission.rs:551-552`) |
| same, with `sv_tools_approval {eval allow}` in a cfg | `dyn: eval runtime_snapshot failed: eval bridge parent session is not bound for this owner` |
| `omp rpc` user-local `bash` running the same `dyn eval` | same `not bound` message |
| `omp chat --py-eval` on a PTY, the `$` prefix (which runs `eval` directly, no `dyn`; `crates/agent/src/local.rs:42-43`), in the daemon and, with `--add-dir`, the embedded environment | `eval runtime_snapshot failed: ... not bound for this owner`, then `Execution failed` |

`/private/tmp/omp-trust-targets/dyneval-owner/debug/omp` (built from the `#283` branch, 2026-10-13 05:32) was also tried. It returned `dyn: eval requires an authenticated issuing session owner` in one session and the `not bound` message in others, so I could not isolate its behavior and make no claim about it.
`target/debug/omp` lacks `#283` entirely (its binary contains neither new message), so the first `dyn eval` there fails one step earlier, on approval policy, and then on "not bound" [INFERENCE: I did not diff the two binaries' behavior].

**Likely cause [INFERENCE, read from source, not confirmed by a patched build].** The parent is bound under one id and the cell's principal carries another:

- Bound under the journal **file stem**: `let id = journal_path.file_stem()...` (`crates/driver/src/headless/kernel.rs:3068-3071`), `bind_eval_sdk_parent(id, parent)` (`:3167`).
- The tool call's `session_id` is **`Hash32::sum(journal_path bytes)`** (`crates/agent/src/dispatch.rs:2206-2210`) and goes to the environment as the principal (`kernel.rs:1269`).
- The bridge parses the owner as `[session, agent]` and looks up `session` among bindings (`crates/envd/src/eval/bridge.rs:1102-1126`), so a hash never matches a stem and `freeze_runtime` fails (`:1128-1133`).
- The tests that pass use the same literal for both: `"test-session"` (`crates/app/tests/it/envd_contract.rs:650-651,1757`; `crates/envd/src/eval/bridge.rs:2058-2061`). I found no production-path test.

I confirmed only that for one real journal the stem and the SHA-256 of its path differ. Whether the hash is the intended principal and the bind site is wrong, or the reverse, is a decision for the owners of `dispatch.rs` and `kernel.rs`. I changed no Rust.

What I did instead: ran the script's control flow (plan validation, rounds, gate, feedback in the retry item, the three-attempt cap) against a **fake** `workpool`/`read`/`output` in plain Python. It exercised only my own loop and says nothing about omp2.
Everything the script asks of omp2 is UNVERIFIED, in particular:

- that `pool.status()["closed"]` plus empty counts means the batches are settled (source: `workpool_scheduler.rs:1086-1095,1545-1549`);
- that `peek()["batches"][i]["output"]` is an `artifact://sha256/<hex>` address whose body is the JSON `{item id: data}` (`:1386-1417`, `yield_assembly.rs:79-128`) and that `output(<that address>)` returns it;
- that a retiring worker's patch lands in the project after `closed` (the script waits up to 90s for the file);
- that the pool item id equals the key in that JSON;
- that a `task` worker obeys "Submit data {...}" via the batch-local `yield` (`workpool_runtime.rs:524-542`).

## 5. Defects in `omp adw` found on the way

1. **`-m/--model` does not override a user `ai_model`.** Model phases compose with `child_kernel_options`, which leaves `model_override` at its default `false` (`production.rs:159-165,235-244`),
   and `apply_model_override` only writes `ai_model` when explicit (`crates/driver/src/headless/kernel.rs:3464-3475`). **Observed:** with `-m omniroute/cohere/command-r7b-12-2024` and a user cfg `ai_model zai/glm-4.7-flash`, the request went to `glm-4.7-flash` and failed `Ambiguous model`.
   Workaround used: `OMP_CONFIG_DIR` pointing at a scratch directory holding only an `ai_model` line and a symlink to `models.toml`. A project `.omp/config.cfg` with `set ai_model ...` did not help.
2. **`on_reject = "correct"` is not bounded by `--max-attempts`** (`run.rs:351-369`; 7 rewinds of one phase observed against a limit of 3).
3. **A transient provider error ends the run with no record** (`mod.rs:160-163`; six observed aborts above), and `PLAN.md` etc. stay on disk.

## 6. What only one of them can do

**Only `omp adw`:** `requires` checks and the `Blocked` exit (`run.rs:249-260`); a trust-gated, sandboxed code phase from a declared argv (`production.rs:380-383,177-226`); typed rewinds with stale-evidence detection (`run.rs:299-315,351-369`);
`omp adw list` and `omp adw run` exit codes with Ctrl-C handling (`adw_cmd.rs:29-110`); a closed, validated schema (`definition.rs:222-250`).

**Only eval:** parallel fan-out across items (adw dispatch is serial: `mod.rs:198-200`); persistent workers reused across items, with a concurrency ceiling from `sv_task_max_concurrency` (`workpool_scheduler.rs:1188-1214`; `settings.rs:123`); per-worker worktree isolation;
a pool state persisted in the session journal (`:992-1082`); arbitrary control flow (a plan that decides how many items to push).
**Neither** has resume. Neither enforces a `Requirement` on a model phase.

## 7. Verdict

- Use `omp adw` today for a serial, gated flow where the gate must be sandboxed and the posture checked, after fixing the three defects in section 5.
- Eval is the only route to parallel writers, but the script here is a design checked against source, not a demonstrated flow. It needs the eval-bridge owner mismatch (section 4) resolved and one real run before anyone relies on it.
- Next step for whoever owns the eval bridge: bind and look up the parent under one id, add a production-path test (compose a kernel, run an eval cell), then run `adw_on_eval.py` once and replace the UNVERIFIED list in section 4 with output.

## 8. Method notes

- State I created and removed: temp repos and scratch configs under `/tmp/adwev-*`; a `[[workspace_trust]]` row in `~/.local/share/omp/ext/grants.toml` (that file did not exist before; deleted after the final run); the per-project state directories under `~/.local/share/omp/{projects,worktrees}` for my temp repos.
- To trust a `/tmp` workspace I wrote the grant row by hand, using a throwaway Python replica of `inventory()`'s digest (`crates/ext/src/workspace_trust/inventory.rs:440-469,841-854`). The run was accepted as trusted, which confirms the replica matched; the replica is not kept.
- Credentials were not read or copied; runs used `OMP_LLM_KEY_SOURCE=local-file`.
