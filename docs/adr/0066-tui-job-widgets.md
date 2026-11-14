# 0066. Job output in `/hub`: a model-free live tail for shell jobs first; widgets, charts and model specs deferred

Status: Proposed 2026-11-14; EGM round 2 resolved, round 3 pending
Date: 2026-11-13 (revised 2026-11-14)
Area: interface

Research: [docs/research/tui-job-widgets.md](../research/tui-job-widgets.md) (Gap 12 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd638`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. Nothing was built or run for this revision.

## Terms

- **Shell job**: a `bash` call that detached into envd, either because it asked for `async` or because it ran past the auto-background threshold. It is a `<job kind="tool">` in the session DOM whose work is an envd **named process**.
- **Named process**: a process envd owns, addressed by a name and a **generation**, a counter that changes on every restart. Its output goes to a bounded log on disk with absolute byte offsets.
- **`/hub`**: the job supervisor overlay. Its **inspector** is the right-hand detail panel for the selected job.
- **`Services`**: the trait through which `omp-chat` reads application data; the app implements it.
- **TSP / Tern**: the native surface protocol and terminal of ADR 0041. A **surface** in this record always means a TSP surface; UI places are called overlays, panels or the status band.

## Context

### The problem

A shell job runs in the background and the user cannot see what it is doing. `/hub` lists the job with its status and age; the inspector shows kind, owner, times and the raw detached payload, but no output (`crates/chat/src/overlays/hub.rs:794-905`). The only way to see output is to ask the model to poll the job.

### What OMPv1 does

- Upstream draws live "job widgets": a cheap model writes a declarative spec per job, a deterministic runtime applies it to each new output line, and renderers draw charts, bars and meters in the `wait` card, the HUD jobs pill and the jobs sheet (`upstream:packages/tui/src/tools/job-widget.ts:1-13`; switch `async.widgets`, default on, `upstream:docs/settings.md:654`).
- Its model-free fallback view is the newest output line plus line counts (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:639-693`). The newest line stands for the job's activity only for a command's own output, never for a subagent's tool results (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:313-320`); that rule fixed a leak of fetched text into widgets (`upstream:packages/coding-agent/CHANGELOG.md:829`).
- Auto-graph adds charts under numeric tables in answers: about 12,000 lines plus a 16.5 KB rubric (`upstream:packages/tui/src/charts/chart-plan.ts:1-8`).

### What omp2 has

- Shell jobs: `bash` with `async`, or a run past `sv_shell_auto_background_threshold_ms` (default 60 s), detaches into an envd named process (`crates/tools/src/shell.rs:716-746`, `crates/envd/src/exec_settings/shell.rs:111-118`, `crates/envd/src/tool_shell.rs:332-381`). The call settles with `{"kind":"detached","id":…,"job":JobRef}` (`crates/agent/src/dispatch.rs:3427-3433`); the session folds it into a `<job kind="tool">` whose `data` prop holds that payload (`crates/session/src/components/jobs.rs:318-366`). `JobRef.owner` is `{"kind":"named_process","name":…,"generation":…}` (`crates/tool/src/lib.rs:2088-2103`, `:2206-2217`). `JobRow` already carries `data` as JSON text (`crates/chat/src/overlays/hub.rs:92-109`). The job's label is the detach description ("named process settlement; detached because …"), not the command (`crates/tools/src/shell.rs:1062-1088`).
- Named `process` jobs (from the `process` tool) are also envd named processes, but their `<job>` has the bare name as id, no `data` and no generation (`crates/driver/src/subagent/hub.rs:800-835`). envd rejects an attach whose generation is not current (`crates/envd/src/exec.rs:2218-2235`).
- Jobs the dispatcher detaches at its 30 s blocking limit have an `AgentLoop { agent_id: "kernel" }` owner and stream into the call's `<result>` text only up to the 64 KiB inline limit (`crates/agent/src/dispatch.rs:107`, `:1048-1085`, `:3435-3451`; `crates/driver/src/headless/kernel.rs:2966`).
- envd's process log: absolute offsets, rotation at 25 MiB, reads capped at 2 MiB, `read_after` clamps a cursor into `[start, end]` (`crates/envd/src/process_log.rs:10-12`, `:148-175`). `attach_output` reads a backlog after a cursor, then subscribes to live output (`crates/envd/src/exec.rs:2078-2125`). The server sends `OutputAttached` (with `log_start_offset`, `log_end_offset`, `rotated`), then the backlog as `ProcessOutput` frames, then one `ProcessState`, then live events (`crates/envd/src/server/dispatch.rs:1081-1120`). Each attach holds one stream quota slot until cancelled (`crates/envd/src/server/dispatch.rs:1077-1088`); a subscriber whose receiver is gone is pruned on the next broadcast (`crates/envd/src/exec.rs:4079-4084`). The client gets `ProcessAttachment::next_event` and `cancel` (`crates/env/src/client.rs:3836-3874`). `EnvClient` has no single-process lookup (`process_info` is on `ExtensionEnvClient` only, `crates/env/src/client.rs:2830`), and `list_processes` returns every process (`crates/env/src/client.rs:2246`). Attaching to an exited generation works (`crates/app/tests/it/envd_contract.rs:3114`).
- Chat reads app data through `Services`; methods default to `ServiceError::Unavailable`; streaming feeds return receivers plus a cancel sender, as `LoginFlow` does (`crates/chat/src/overlays/services.rs:22-47`, `:463-475`, `:1234-1240`). The production `AppServices` holds an `EnvClient` and a runtime handle (`crates/app/src/chat_services/mod.rs:85-103`, `:178`). Panels get `PanelCx` (with `services`) only in `open`; `tick(now)` and `next_wake()` take no context (`crates/chat/src/overlays/mod.rs:262-274`; `crates/chat/src/overlays/hub.rs:343-368`, `:1349-1355`). `/hub` is rebuilt from its root on each change and dropped on close; the inspector shows only at 96 columns or more, or with narrow details toggled (`crates/chat/src/overlays/hub.rs:39`, `:609-617`, `:996-1054`).
- The `bash` card already windows live output: the last 10 lines behind an "N earlier lines" marker (`crates/chat/src/cards/bash.rs:11-40`), drawn in `<pre fg=output>` (`crates/chat/src/cards/bash.rs:129`), whose component decomposes ANSI once at its boundary (`crates/tui/src/components/text.rs:498`, `crates/tui/src/rich.rs:182`).
- Chat presentation convars are `cl_*`, declared in `crates/chat/src/settings.rs` (block at `:37`; `CL_GOAL_STATUS_IN_FOOTER` at `:407`). `/hub` already reads one with `cx.con.get_typed::<bool>` at open (`crates/chat/src/overlays/hub.rs:361-364`).
- The real-PTY harness runs the real `omp chat` with a fake provider, the debug socket, a pinned Unicode charset and a hermetic `config.cfg` (`crates/e2e/tests/p7_tui.rs:73-85`, `:740-790`), and checks terminal restoration (`crates/e2e/tests/p7_tui.rs:975`).

### Rules that bind

- Additive only: no change to the `<job>`/`<result>` schema, existing tools, convars or the role vocabulary (`crates/catalog/src/selection.rs:172-178`).
- Deterministic: no model in this plan.
- Layers: client protocol helpers in `omp-env`, host behaviour in `omp-envd`, presentation in `omp-chat`, composition in `omp-app` (`AGENTS.md` Architecture and Conventions).
- TUI doctrine: external text decomposed once at a boundary; no hardcoded glyphs or colours (`AGENTS.md:440-480`). TUI changes get a real-PTY proof: input, resize, clean quit (`AGENTS.md:566`, `:783`, `:821-822`). Done means compiled, wired, exercised (`AGENTS.md:564-565`).
- Errors follow ADR 0035: typed variants, rendered once at the boundary.

## Decision

**D1. Scope: one slice.** This record commits to S1 only: the live output tail of the selected shell job in the `/hub` inspector. Widgets (sparklines, meters, a rule runtime, the chip glance), dispatcher-detached calls, named `process` jobs, model-written specs and table charts are deferred. Each needs its own design; the round-2 review found that the earlier, larger plan was not implementable as written (see "What the review changed"). The alternative, making S1 to S5 implementable in this record, is O1.

**D2. Gate.** `cl_job_widgets` (bool, default `false`, flags `archive`, meta `ui.tab: "tasks"`, `ui.label: "Job Output in Hub"`), declared in `crates/chat/src/settings.rs`. `/hub` reads it once in `open`, as it reads `cl_task_show_resolved_model_badge`. Off: `/hub` is byte-for-byte unchanged and no feed is opened. No `legacy.path`: upstream's `async.widgets` also covers model-written widgets and surfaces omp2 does not have, so importing it would claim more than S1 does. No `cap-*` feature: no new dependency. When to default it on is O2.

**D3. A tail helper in `omp-env`.** `EnvClient::tail_output(TailOutput { name, generation, bytes }) -> Result<OutputTail, ClientError>`, with `bytes` clamped to `1..=64 KiB`:

1. Probe: `attach_output { after_sequence: u64::MAX, max_bytes: 1 }`. `read_after` clamps the cursor to the log end, so the backlog is empty; `OutputAttached` gives `log_start_offset` S and `log_end_offset` E. Cancel the probe.
2. Follow: `attach_output { after_sequence: max(S, E − bytes), max_bytes: bytes + 64 KiB }`. Its backlog ends at the first `ProcessState` frame. If the backlog's last `sequence` is below this attach's own `log_end_offset` (the log grew more than 64 KiB between the calls), cancel and repeat step 2 from the new end, at most twice; after that, keep the newest attach and report `skipped: true`.
3. `OutputTail::next_event()` yields `Backlog { data, start, rotated, skipped }` once, then `Output { data, offset }` and `State(ProcessState)` in order. `OutputTail::cancel(self)` cancels the attachment, which releases the server's stream slot and leaves at most one dead subscriber until the next broadcast prunes it.

Errors are the existing `ClientError` variants (unknown process, stale generation, transport). No new protocol message and no envd change.

**D4. The feed: `Services::job_output`.** A defaulted method (default `Unavailable`):

```rust
fn job_output(&self, target: JobOutputTarget) -> ServiceResult<JobOutputFeed>;
pub struct JobOutputTarget { pub name: Str, pub generation: u64 }
pub struct JobOutputFeed { pub window: Arc<parking_lot::Mutex<OutputWindow>>, pub cancel: flume::Sender<()> }
```

`AppServices` spawns one task on its runtime that drives `tail_output(bytes: 16 KiB)` and pushes every event into the shared `OutputWindow`. The task ends, cancelling the tail, when `cancel` fires or is dropped. A terminal error is stored in the window. Nothing is sent per event: chat reads the window when it ticks.

**D5. `OutputWindow`: the bounded text model, in `omp-chat`.** A plain type in `crates/chat/src/overlays/job_output.rs`, unit-tested without I/O. It holds:

- `text: String`, normalized and bounded. Bytes are decoded as UTF-8, carrying an incomplete trailing sequence to the next chunk (the rule `OutputStream::decode` uses, `crates/agent/src/dispatch.rs:999-1016`; S1 moves that function into a shared helper rather than copying it). CRLF becomes LF. A lone CR keeps only the text after it on that line, which is what a terminal shows after a progress redraw. A line longer than 4096 bytes keeps its last 4096 (upstream's cap, `upstream:packages/coding-agent/src/job-widgets/runtime.ts:36`). When the backlog starts after the log start, everything up to and including its first LF is dropped, so a window that starts mid-line or mid-character never shows a fragment. Whole lines beyond 16 KiB are dropped from the front. The trailing line without LF is kept, so prompts and progress bars show.
- `revision: u64`, bumped on every change.
- `state`: running, exited (with code), stopped or failed, from `ProcessState`.
- `skipped: bool` (D3 step 2, or `rotated` past the cursor).
- `opened_at` and `last_output_at: Option<Instant>` on `tokio::time::Instant`, so tests run on paused time.
- `error: Option<ServiceError>`.

Work per chunk is proportional to the chunk; memory stays under 20 KiB per window.

**D6. The inspector "Output" section.** For the selected job, `/hub` opens a feed when all of these hold: the gate is on, `Section::Agents` shows the inspector, and the job's `data` parses to a `named_process` owner. Subagent jobs, dispatcher-detached jobs (`agent_loop` owner) and named `process` jobs (no `data`) get no feed.

- `AgentHub` keeps a clone of `cx.services` from `open` and at most one feed, keyed by job id. Changing the selection cancels the old feed and opens the new one; closing `/hub` drops it. So an open `/hub` holds at most one envd attachment, and a closed one holds none.
- `next_wake` returns the earlier of `self.ui.next_wake()` and one second after the last check while a feed is held. `tick` locks the window and rebuilds when `revision` changed, or when the quiet text below needs a new second; it returns `true` only then.
- The section sits after "elapsed" and before "Data". The heading is "Output". The body is the bash card's window over `OutputWindow.text` (the last 10 lines behind the "N earlier lines" marker; `output_tail` becomes `pub(crate)` and is reused, not copied), drawn in `<pre fg=output>` so ANSI is decomposed once by the component. A muted status line follows: `quiet 12s` once no output has arrived for 5 s, measured from `last_output_at`, or `quiet ≥12s` when nothing arrived since the feed opened, because the log has no timestamps; `output skipped` when `skipped`; the process state once it is no longer running. An empty window shows `no output yet`. An error shows `output unavailable` and the rendered error, muted.
- TSP: `/hub` is an overlay, and overlays are cell-rendered while a surface is live (ADR 0041 decision 10). No TSP node is involved.

## Rejected options

| Option | Why not |
|---|---|
| Keep the S1-S5 widget plan in this record | Round 2 found two blockers and eleven majors in S2-S5 (chip polling with no owner, `<spark>` body vs the markup parser, undefined rule data model, speculative `Spark` caller). Recorded as O1 for the owner instead. |
| Poll `job_output` once a second with a fresh attach | Every attach subscribes; a quiet job would pile up dead subscribers, about 3,600 per hour (`crates/envd/src/exec.rs:2109`, `:4079-4084`). One held attachment per open `/hub` costs one stream slot. |
| `list_processes` to find the log end | It returns every process with its full spec (`crates/env/src/client.rs:2246`); the probe attach gives the offsets for one process. |
| Compose the tail inside `AppServices` | Client protocol helpers belong in `omp-env`; `AppServices` only adapts. |
| Show the newest line in every roster row | One attachment per running job and a layout change in two roster views; the inspector needs one. Deferred with the widgets. |
| Read the DOM `<result>` stream | Shell jobs never stream into it after detach, and it keeps only the first 64 KiB (`crates/agent/src/dispatch.rs:1048-1085`). |
| Strip ANSI in the feed | The `<pre>` component already decomposes once (`crates/tui/src/components/text.rs:498`); stripping earlier would decompose twice and lose colour. |
| Ship S1 with no gate | The gap-closure brief requires a default-off gate until the acceptance tests pass. |

## Slices

**S1. Live output tail of the selected shell job in the `/hub` inspector.**
- Scope: `cl_job_widgets`; `EnvClient::tail_output`, `TailOutput`, `OutputTail` (`crates/env/src/client.rs`); `Services::job_output`, `JobOutputTarget`, `JobOutputFeed` (`crates/chat/src/overlays/services.rs`); `OutputWindow` (`crates/chat/src/overlays/job_output.rs`); the shared UTF-8 carry helper moved out of `crates/agent/src/dispatch.rs`; `AppServices::job_output` (`crates/app/src/chat_services/mod.rs`); the inspector section and feed lifecycle in `crates/chat/src/overlays/hub.rs`; `output_tail` made `pub(crate)` in `crates/chat/src/cards/bash.rs`.
- Unit, `crates/chat/src/overlays/job_output.rs`: `utf8_split_across_chunks_decodes_once`, `crlf_and_lone_cr_keep_what_a_terminal_shows`, `line_over_4096_bytes_keeps_its_tail`, `backlog_starting_mid_log_drops_the_leading_fragment`, `window_keeps_16_kib_of_whole_lines_plus_the_open_line`, `revision_changes_only_when_text_or_state_changes`.
- Unit, `crates/chat/src/overlays/hub.rs` (fake `Services`, paused time): `gate_off_opens_no_feed_and_renders_unchanged`, `named_process_owner_opens_one_feed`, `subagent_kernel_and_process_jobs_open_no_feed`, `selection_change_cancels_the_old_feed`, `closing_hub_cancels_the_feed`, `quiet_marker_uses_lower_bound_until_output_arrives`, `unavailable_feed_shows_output_unavailable`, `tick_repaints_only_on_revision_or_quiet_second`.
- Crate integration, `crates/app/tests/it/envd_contract.rs` (real envd, beside the existing `attach_output` cases at `:3078-3170`): `tail_output_returns_bytes_before_log_end`, `tail_output_retries_when_log_grows_past_the_window`, `tail_output_cancel_releases_the_stream_slot` (open and cancel more tails than the connection's stream quota, in sequence, without error), `tail_output_reads_an_exited_generation`, `tail_output_rejects_a_stale_generation`.
- Real PTY, `crates/e2e/tests/p7_tui.rs`: `chat_tui_hub_shows_live_job_output_through_resize_and_clean_quit`. Seed `config.cfg` with `cl_startup_check_update 0` and `cl_job_widgets 1` (the harness writes its default only when the file is absent). PTY 40×120 so the inspector shows. The fake provider makes one `bash` call with `async: true` whose command is `for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do echo tick-$i; sleep 0.5; done`, using the in-process `echo` and `sleep` builtins. Keys `/hub` Enter, select the job; wait for `tick-3`, then for `tick-6` (proving live update); resize to 30×100 and wait for the repaint with `tick-` still in the inspector; `C-c C-c`; `assert_restored`.
- Acceptance: gate on, the selected shell job's newest output lines appear in the inspector and update while `/hub` is open; gate off, the existing `/hub` tests and all P7 cases pass unchanged; no attachment outlives `/hub`.
- Gate: `cl_job_widgets`, default off. Depends on: none. Verify: `just test-pkg omp-env`, `just test-pkg omp-chat`, `just test-pkg omp-app`, `just e2e-p7`, `just lint`.

## Deferred work (not designed here)

Each item needs its own record or an amendment once S1 has run. Each note records what the round-2 review established, so that work starts from checked facts.

- **Sparkline and `<spark>`.** A tag with a body must go through the raw-body branch of the markup parser, not `is_leaf_tag`, which drops the body (`crates/tui/src/markup.rs:523-552`, `:579-582`). `dom!` needs a `spark => Spark;` entry in `for_each_component!` (`crates/vocab/src/lib.rs:238-270`). Colour cannot be "as `Progress`", which paints theme tokens directly and ignores `fg` (`crates/tui/src/components/progress.rs:100-106`). Two ramps are already hardcoded and would move to the new charset ramp: `trend_bar` in `omp usage` (`crates/app/src/usage_cmd.rs:350-364`) and the live voice meter (`crates/chat/src/overlays/live.rs:667`). The TSP `chart` kind exists on the wire with no schema or builder in omp2 (`crates/tui/src/tsp/wire.rs:108`), so a lowering needs ADR 0041 work first.
- **Status-band glance.** The band is not a panel and has no `tick` (`crates/chat/src/status_band.rs:1994-2000`). A glance there needs an owner in the chat actor, a cadence and a budget, and would end S1's "no attachment without `/hub`" property.
- **Rule-driven views for known commands.** Start from the deterministic patterns `omp-minimizer` already has for `cargo` and `pytest` output (`crates/minimizer/src/filters/cargo.rs:130-160`, `crates/minimizer/src/filters/python.rs`), as small typed extractors, before porting upstream's spec language. The command text is not on the `<job>` (`crates/tools/src/shell.rs:1062-1088`); it would come from the originating call's arguments.
- **Dispatcher-detached calls** (`agent_loop` owner). Their DOM text is a 64 KiB head. Showing it as "activity" for non-shell tools would reintroduce upstream's fetched-text leak. Whether a foreground `bash` is first detached by the dispatcher at 30 s or by the shell at 60 s is unverified.
- **Named `process` jobs.** They need a generation: either a `<job>` prop (a schema change, owner decision) or a name-only lookup on `EnvClient` (an additive `omp-env` method).
- **Model-written specs** (upstream's author with the `widget` role). No `widget` role exists (`crates/catalog/src/selection.rs:175-178`), and there is no seam between chat and driver for samples and specs.
- **Table charts** (auto-graph).

## Upstream origin and license

S1 ports no upstream code. It follows upstream's model-free fallback idea (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:639-708`) and its 4096-byte line cap (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:36`). Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Any later port of `upstream:packages/coding-agent/src/job-widgets/` or `upstream:packages/tui/src/components/sparkline.ts` records its origin in its own record.

## Consequences

- A user can watch a background shell job's output in `/hub` without asking the model.
- An open `/hub` holds at most one envd output attachment; a closed one holds none.
- `omp-env` gains a reusable tail-and-follow helper for named processes.
- Upstream's widgets, glance and charts stay absent; omp2 users get output lines, not charts, until the deferred work is designed.

## Open questions for the owner

- **O1. Scope.** This record cuts to S1 (zoom-out's recommendation). The other reviewers wanted S1 to S5 made implementable here: rule runtime, sparkline, chip glance and model specs. Recommendation: S1 now; decide on the widget work after S1 has been used, starting from the deferred notes.
- **O2. Default.** When does `cl_job_widgets` default on? Recommendation: after S1's tests have passed in CI for one release and the owner has used it; upstream defaults its widgets on.
- **O3. Named `process` jobs.** Add a generation prop to their `<job>` (schema change) or a name-only lookup to `EnvClient` (additive)? Recommendation: the lookup.
- **O4. `widget` role**, if model specs are ever built: a new role or an alias of `smol`? Recommendation: the alias, which leaves the role vocabulary unchanged.

## What the review changed

### Round 1 (2026-11-13; findings as recorded in `docs/adr/STATUS.md` row 0066)

- **No real-PTY proof per slice.** Confirmed: `AGENTS.md:566`, `:783`, `:821-822`; the 2026-11-13 slices named only unit and snapshot tests. Changed: every slice got a `crates/e2e/tests/p7_tui.rs` case (input, resize, `C-c C-c`, `assert_restored`, `crates/e2e/tests/p7_tui.rs:975`). After round 2, S1's case is the only one.
- **Sparkline ramp API and `<spark>` tag path unspecified.** Confirmed: one sentence in the old D2; `crates/tui/src/markup.rs:1074-1330` registers tags; no series prop in `crates/vocab/src/lib.rs:36-38`. Changed in round 1: a full D6 spec. Round 2 found that spec wrong (S-02, S-13, S-17), so it now survives only as the deferred notes. Kept from round 1: upstream's mapping draws a constant positive series at the top block (`upstream:packages/tui/src/components/sparkline.ts:29-43`), not the lowest as the first draft's test said.
- **Output tail and chip detail surface unspecified.** Confirmed: the DOM `<result>` holds only the first 64 KiB (`crates/agent/src/dispatch.rs:107`, `:1048-1085`); shell jobs never stream into it after detach (`crates/tools/src/shell.rs:716-746`, `crates/envd/src/tool_shell.rs:373-381`); the chip has no detail surface (`crates/chat/src/status_band.rs:1994-2000`). Changed: the tail comes from envd's process log (round 2 respecified how, D3-D5); the chip idea was dropped.
- **S1 dead code until S3.** Confirmed: upstream's fallback has no spark (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:671-693`). Changed in round 1: the sparkline moved behind the job view. Round 2 dropped it from the committed plan.
- **TSP lowering (coordinator note on ADR 0041).** Confirmed: `crates/chat/src/tsp.rs:836-850`; `chart`/`meter` kinds at `crates/tui/src/tsp/wire.rs:108-109`; overlays are cell-rendered while a surface is live (ADR 0041 decision 10). Changed: D6 states that `/hub` sends no TSP node; the `chart` lowering is deferred (S-17).
- **Corrected against this tree.** `sv_job_widgets` became `cl_job_widgets` (`crates/chat/src/settings.rs:37`, `:407`). Area `ui` became `interface`.

### Round 2 (2026-11-14; four fresh-context reviewers plus a synthesizer, 2 blockers, 13 majors, 6 minors)

Every finding was checked in this tree. Scope decision: **cut to S1** (zoom-out's recommendation) and record the larger plan as O1. The cut resolves S-01, S-02, S-09 to S-14 and parts of S-17 by removing the designs they faulted. The checked facts are kept in "Deferred work" so a later design starts from them.

- **S-01 (blocker) chip glance needs polling with `/hub` closed.** Confirmed: the status band is not a panel and has no `tick` (`crates/chat/src/status_band.rs:1994-2000`; `Panel::tick` at `crates/chat/src/overlays/hub.rs:1349`); the hub is created per open (`crates/chat/src/overlays/hub.rs:343`). Resolved: the glance is deferred; S1 holds no attachment without `/hub` (D6) and the Consequences now say so.
- **S-02 (blocker) `<spark>` body vs the leaf-tag rule.** Confirmed: leaf tags finish with an empty body (`crates/tui/src/markup.rs:579-582`); body tags use the raw-body branch (`crates/tui/src/markup.rs:523-552`). Resolved: the sparkline is deferred, and the correct path is in the deferred notes.
- **S-03 `job_output` not implementable; polling leaks subscribers.** Confirmed: each attach pushes a subscriber (`crates/envd/src/exec.rs:2109`), pruned only on broadcast (`:4079-4084`); backlog end is marked by the first `ProcessState` (`crates/envd/src/server/dispatch.rs:1096-1112`); `process_info` is not on `EnvClient` (`crates/env/src/client.rs:2830`); `list_processes` is unfiltered (`:2246`). Resolved: D3 holds one attachment, with a probe for the offsets, a stated window formula, a growth retry, a backlog-end rule and cancel; the helper lives in `omp-env`; D4 shares a bounded window instead of returning one-shot results.
- **S-04 label and command text are not what the ADR assumed.** Confirmed: `crates/tools/src/shell.rs:1062-1088`. Resolved: S1 uses neither. The inspector already titles the job by id, and command-keyed rules are deferred, with the source of the command noted.
- **S-05 `JobRow` lacks owner; no generation for `process` jobs.** Confirmed: `crates/driver/src/subagent/hub.rs:828-835`; generation must match (`crates/envd/src/exec.rs:2227-2233`). Resolved: S1 reads the owner from the existing `JobRow.data` (`crates/agent/src/dispatch.rs:3427-3433`, `crates/tool/src/lib.rs:2088-2103`) with no `JobRow` change; `process` jobs are out of S1 and are O3. Round 1's "O4 resolved" was wrong and is withdrawn.
- **S-06 line and byte semantics incomplete.** Confirmed: `crates/envd/src/process_log.rs:148-175`; upstream's 4096 cap at `upstream:packages/coding-agent/src/job-widgets/runtime.ts:36`. Resolved by D5: UTF-8 carry, CRLF and CR, the line cap, the leading fragment, the bound, the open trailing line. Lag cannot arise, because the task folds each event as it arrives into a fixed window. On rotation and skips, the `skipped` flag shows.
- **S-07 rate and quiet semantics undefined; state lifetime.** Confirmed: the hub is dropped on close and uses a wall clock (`crates/chat/src/overlays/hub.rs:163-168`). Resolved: the rate is removed. Quiet is a stated lower bound (`quiet ≥Ns`) on `tokio::time::Instant`. State lives in the feed and dies with `/hub`.
- **S-08 polling plumbing through `Panel::tick` unspecified.** Confirmed: `crates/chat/src/overlays/mod.rs:262-274`; `crates/chat/src/overlays/hub.rs:343-368`, `:1349-1355`. Resolved by D6: the services clone is taken in `open`, at most one feed is held, the `next_wake` combination and the `tick` repaint rule are stated, and tests inject a fake `Services`.
- **S-09 roster-line change not mapped to layouts.** Confirmed: `crates/chat/src/overlays/hub.rs:743-780`. Resolved: the roster is unchanged in S1 (Rejected options).
- **S-10 dispatcher-detached output, leak, owner transition, 30 s PTY cost.** Confirmed: `agent_loop` owner at `crates/agent/src/dispatch.rs:3435-3451`; 64 KiB head at `:1048-1085`. Resolved: deferred, with the leak risk and the unverified 30 s vs 60 s order noted. S1 excludes those jobs by owner kind.
- **S-11 rule runtime unspecified and unjustified.** Confirmed: `crates/minimizer/src/filters/cargo.rs:130-160` already recognises `cargo test` summaries. Resolved: deferred, with the note to start from minimizer patterns as typed extractors.
- **S-12 `Spark` built for a contrived caller.** Accepted, as a scope judgment backed by `upstream:packages/coding-agent/src/job-widgets/runtime.ts:671-693`. Resolved: deferred.
- **S-13 spark colour cites `Progress` wrongly.** Confirmed: `crates/tui/src/components/progress.rs:100-106` paints theme tokens and never reads `fg`. Resolved: recorded in the deferred notes.
- **S-14 D8, D9, S5, S6 are speculative detail.** Accepted. Resolved: reduced to one-line deferred notes and O4.
- **S-15 root problem unstated; S1 model too large.** Confirmed: the bash card's window exists (`crates/chat/src/cards/bash.rs:11-40`). Resolved: Context opens with the problem; `JobWidgetView` and its states are gone; S1 reuses `output_tail`.
- **S-16 no flip criterion; loose registration.** Confirmed: `crates/chat/src/settings.rs:400-412`; open-time read precedent at `crates/chat/src/overlays/hub.rs:361-364`. Resolved: D2 gives the meta, read time and why there is no `legacy.path`; O2 covers flipping the default. **Rejected in part**: shipping S1 ungated, because the gap-closure brief requires a default-off gate until acceptance passes.
- **S-17 spark follow-ups.** Confirmed: `crates/vocab/src/lib.rs:238-270`; `crates/app/src/usage_cmd.rs:350-364`; `crates/chat/src/overlays/live.rs:667`; `crates/tui/src/tsp/wire.rs:108`. Resolved: all in the deferred notes. The round-1 Context line "no sparkline" is corrected there: two hardcoded ramps exist.
- **S-18 ANSI decomposition location inconsistent.** Confirmed: `crates/tui/src/components/text.rs:498`. Resolved: the feed passes text through; `<pre>` decomposes once, as for the bash card (Rejected options).
- **S-19 PTY case ambiguous and under-seeded.** Confirmed: `config.cfg` is written only when absent (`crates/e2e/tests/p7_tui.rs:744-753`). Resolved: S1 gives the exact command, markers, seed lines, sizes and steps.
- **S-20 terms undefined; altitude.** Accepted in part. Resolved: a Terms section; mapping formulas and per-component test lists left with the deferred work; O1, O4 and O5 of round 1 removed because the Decision settles them. **Rejected in part**: Context and "What the review changed" stay at this length, because the brief requires the evidence and one entry per finding.
- **S-21 header vs `docs/adr/STATUS.md` row disagree.** Confirmed: `docs/adr/STATUS.md:78`. **Rejected for this change**: the orchestrator owns `STATUS.md` and updates the row; this record is the newer source.

Round 3: pending. Not owner-approved.

## Acceptance tests

1. S1: with `cl_job_widgets 1`, a running `bash` `async` job's newest output appears in the `/hub` inspector and updates while `/hub` is open, on a real PTY, through a resize and a clean quit.
2. S1: with `cl_job_widgets 0`, no feed is opened and the existing `/hub` tests and P7 cases pass unchanged.
3. S1: closing `/hub` or changing the selection cancels the attachment, and the stream slot is released (`tail_output_cancel_releases_the_stream_slot`).
4. S1: output split across chunks, CR redraws, over-long lines and a backlog starting mid-log render as D5 states.
5. S1: subagent, dispatcher-detached and named `process` jobs open no feed.

## Status in omp

**Status: Not started.** Reusable today: `EnvClient::attach_output` and `ProcessAttachment::cancel`, the `Services`/`LoginFlow` feed pattern, the `/hub` inspector, the bash card's `output_tail`, `<pre>` ANSI decomposition, the P7 PTY harness.

## References

[docs/research/tui-job-widgets.md](../research/tui-job-widgets.md); ADR 0005 (the hub); ADR 0012 (convars); ADR 0035 (errors); ADR 0041 (surfaces over TSP); ADR 0008 and 0009 (tool output streaming and bounds).
