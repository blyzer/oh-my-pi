# 0066. Job widgets: a model-free live job row first, then a deterministic rule runtime; auto-graph charts deferred

Status: Proposed 2026-11-14; EGM round 1 resolved, rounds 2-3 pending
Date: 2026-11-13 (revised 2026-11-14)
Area: interface

Research: [docs/research/tui-job-widgets.md](../research/tui-job-widgets.md) (Gap 12 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd638`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. This record states decisions; the note holds the evidence. Nothing was built or run for this revision.

## Context

### What OMPv1 does

- Job widgets: a widget is data, not code. A cheap model writes a declarative spec once per job; a deterministic runtime applies it to each new output line; renderers draw the view as TSP nodes or ANSI lines (`upstream:packages/tui/src/tools/job-widget.ts:1-13`). Surfaces are the `wait` card, the HUD jobs pill and its hover card, and the jobs sheet; the switch is `async.widgets`, default on (`upstream:docs/settings.md:654`).
- A spec has up to 12 rules and 4 blocks (`upstream:packages/coding-agent/src/job-widgets/spec.ts:157-160`). A rule is a regex line matcher with named groups feeding fields, reduced by one of ten aggregates (`upstream:packages/coding-agent/src/job-widgets/spec.ts:14`, `upstream:packages/coding-agent/src/job-widgets/spec.ts:47-62`). Blocks are spark, bars, meter, stat, grid and text (`upstream:packages/tui/src/tools/job-widget.ts:51-120`). Validation requires every regex to compile, every read field to be fed, and the rules to match the samples the author saw (`upstream:packages/coding-agent/src/job-widgets/spec.ts:1-7`).
- The runtime uses no model past the spec and does work proportional to new text with bounded rings: 240 points per series, 48 drawn after downsampling, a 60 s rate window (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:1-8`, `upstream:packages/coding-agent/src/job-widgets/runtime.ts:36-42`). The registry refreshes at 1 s while watched, allows a spec request only after 15 s and at most 2 in flight, and re-authors on drift (`upstream:packages/coding-agent/src/job-widgets/registry.ts:17-45`).
- The fallback view (no spec) is a title, the newest non-empty output line as the stage, and one `stat` block with `lines` and `lines/s`; it has no series and no spark (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:639-693`). The newest line stands for the stage only for a command's own output, never for a subagent's tool results (`lineStage`, `upstream:packages/coding-agent/src/job-widgets/runtime.ts:313-320`). That rule is the fix for a leak where widgets showed text fetched by tools (`upstream:packages/coding-agent/CHANGELOG.md:829`); percentage meters needed special handling too (`upstream:packages/coding-agent/CHANGELOG.md:1154`).
- The HUD glance shows only a widget a spec drives (`state: "live"`), newest first: its headline or first spark/meter figure plus an 8-cell spark of the newest values (`upstream:packages/tui/src/tools/job-widget.ts:203-240`, `upstream:packages/tui/src/tools/job-widget.ts:264-279`). Fallback views never reach the pill.
- The sparkline maps a 0-1 fraction to one of eight blocks `▁▂▃▄▅▆▇█` by `floor(fraction × 8)`, clamped; the domain defaults to `[0, largest value]`; a zero-span domain draws the full block for a value above `min` and the lowest otherwise; non-finite values draw a gap; `fitSparkValues` squeezes a long series into equal-count bucket means, newest last (`upstream:packages/tui/src/components/sparkline.ts:6-66`).
- On TSP a spark block lowers to a `chart` node with `kind`, `series`, `min`, `max`, `marks`, `token` and `summary` (`upstream:packages/tui/src/tools/job-widget.ts:733-758`; props at `upstream:packages/wire/src/tsp.ts:945-972`).
- Auto-graph: numeric tables in answers get a chart on graphics terminals, planned locally and optionally refined by the judge (`upstream:docs/settings.md:917`, `upstream:packages/tui/src/charts/chart-plan.ts:1-8`). About 12,000 lines of chart code plus a 16.5 KB rubric.

### What omp2 has

- `omp-tui` has `Progress` (the `<progress>` tag, glyphs from `Charset::progress`, colour from theme tokens), a charset layer (`Unicode`, `NerdFont`, `Ascii`) and a rule against hardcoded colours and glyphs (`crates/tui/src/components/progress.rs:10-110`, `crates/tui/src/context.rs:41-49`, `crates/tui/src/context.rs:220-225`, `AGENTS.md:460-462`). Markup tags are registered in three places in `crates/tui/src/markup.rs`: the catalog list `is_catalog_tag` (`:1074-1130`), `is_leaf_tag` (`:1155-1175`) and the `build` table (`:1277-1330`, `"progress"` at `:1319`). The vocabulary has `min`, `max`, `value` and `label` props but no series prop (`crates/vocab/src/lib.rs:36-38`, `:111`, `:117`). No sparkline, no chart component.
- The TSP wire already has `chart` and `meter` kinds (`crates/tui/src/tsp/wire.rs:108-109`) but no typed builder for them. A card without a description is sent as the generic `tool` node, then `rows` (`crates/chat/src/tsp.rs:836-850`; ADR 0041 decision 7). While an overlay is open the surface is suspended and the cell renderer draws it (ADR 0041 decision 10). The status band goes out as one `status` node split into `seg` nodes on `·` (`crates/chat/src/tsp.rs:792-810`).
- Jobs live in the session DOM under `<meta><jobs>`; `/hub` reads each row into `JobRow` (`crates/agent/src/jobs.rs:1-6`, `crates/chat/src/overlays/hub.rs:92-139`). The hub has a roster line per job and an inspector for the selected one (`crates/chat/src/overlays/hub.rs:731-800`); it refreshes only when `job_rows` change (`crates/chat/src/overlays/hub.rs:1316-1324`) and has `tick`/`next_wake` hooks (`crates/chat/src/overlays/hub.rs:1349-1355`). The status band shows a `Jobs` count chip and nothing more: no hover, no detail (`crates/chat/src/status_band.rs:1994-2000`). The `wait` card renders settled job snapshots only; while the call runs it shows `poll <id>` (`crates/chat/src/cards/hub.rs:437-480`).
- Where a running job's output lives depends on how it detached:
  - **Shell jobs** (`bash` with `async`, and auto-backgrounded runs past `sv_shell_auto_background_threshold_ms`, default 60 s) become envd named processes: the tool returns `ToolTerminal::Detached` with a `NamedProcess { name, generation }` owner and job id `name#generation` (`crates/tools/src/shell.rs:716-746`, `crates/envd/src/tool_shell.rs:332-381`, `crates/envd/src/exec_settings/shell.rs:111-118`, `crates/tool/src/lib.rs:2088-2103`). The session folds that into a `<job kind="tool">` with the detached payload in `data` and the call id in `call` (`crates/session/src/components/jobs.rs:318-366`). Their output after detach is in envd's process log, not in the DOM. `EnvClient::attach_output` returns the log from any offset, capped at `max_bytes` (2 MiB ceiling), then follows live output (`crates/env/src/client.rs:2272-2280`, `crates/envd/src/exec.rs:2078-2110`, `crates/envd/src/process_log.rs:12`, `crates/envd/src/process_log.rs:148-165`). Named `process` jobs are the same kind of envd process (`crates/driver/src/subagent/hub.rs:800-935`).
  - **Dispatcher-detached calls** (a tool still running at the 30 s blocking limit, `crates/driver/src/headless/kernel.rs:2966`) keep streaming frames into the call's `<result>` text, but only up to the inline limit (64 KiB by default); the rest goes to a spill stage that becomes readable only when the call closes (`crates/agent/src/dispatch.rs:107`, `crates/agent/src/dispatch.rs:939-1111`). So the DOM holds the head of a long output, never its tail. The call element is found by its `id` prop, the job's `call` value (`crates/session/src/fold.rs:690-693`).
- Chat reaches application data only through `Services`, whose methods default to `ServiceError::Unavailable` and return a `Pending` receiver that a panel polls from `tick` (`crates/chat/src/overlays/services.rs:22-47`, `crates/chat/src/overlays/services.rs:1234-1240`, `crates/chat/src/overlays/mod.rs:262-274`). The production implementation holds an `EnvClient` and a runtime handle (`crates/app/src/chat_services/mod.rs:85-103`, `crates/app/src/chat_services/mod.rs:178`).
- Chat presentation convars are `cl_*`, declared in `crates/chat/src/settings.rs` (block at `:37`, e.g. `CL_GOAL_STATUS_IN_FOOTER` at `:407`).
- `regex`, `resvg` and `image` are workspace dependencies; `omp-tui` does not use `resvg` (`Cargo.toml:215`, `Cargo.toml:217`, `crates/tui/Cargo.toml:28-36`). Built-in model roles include `smol`, `tiny`, `judge`, `memory`, no `widget` (`crates/catalog/src/selection.rs:175-178`).
- The real-PTY harness runs the real `omp chat` on a PTY with a fake provider, the debug socket and a pinned Unicode charset, and checks terminal restoration (`crates/e2e/tests/p7_tui.rs:73-85`, `crates/e2e/tests/p7_tui.rs:740-790`, `crates/e2e/tests/p7_tui.rs:975`).

### Rules that bind

- Deterministic rules first, a model only where rules cannot.
- Additive only; the role vocabulary is a public contract (`crates/catalog/src/selection.rs:172-178`). The DOM schema of `<job>` and `<result>` does not change in this plan.
- TUI doctrine: effects are reusable props or components in core, charset-aware and themed; external text is decomposed once at the boundary (`AGENTS.md:440-480`).
- TUI changes are proven on a real PTY: input, resize, clean quit (`AGENTS.md:566`, `AGENTS.md:783`, `AGENTS.md:821-822`).
- Every slice is compiled, wired and exercised when it lands (`AGENTS.md:564-565`).
- Errors follow ADR 0035; secrets must not reach a model.

## Decision

**D1. Stage the work.** Model-free parts ship first; a model-written spec ships last and only if the surface proves wanted. Every slice lands with a caller and a real-PTY proof.

**D2. Gate.** All presentation behaviour is behind the default-off convar `cl_job_widgets` (bool, `archive`), declared in `crates/chat/src/settings.rs`. With it off, `/hub`, the status band and every card are unchanged. Model authoring has its own default-off `sv_job_widgets_author` (D8). Table charts, if ever built, use `cl_table_charts` (D9). No `cap-*` feature: nothing here adds dependency weight (`regex` is already a workspace dependency).

**D3. The output tail feed.** `Services` gains one defaulted method (default `ServiceError::Unavailable`, like every other feed):

```rust
fn job_output(&self, request: JobOutputRequest) -> ServiceResult<Pending<JobOutput>>;

pub struct JobOutputRequest { pub name: Str, pub generation: u64, pub after: Option<u64>, pub max_bytes: u32 }
pub struct JobOutput { pub text: Str, pub start: u64, pub end: u64, pub log_end: u64, pub rotated: bool, pub running: bool }
```

- `after: None` reads the tail: the last `max_bytes` (at most 16 KiB) before the log end. `after: Some(offset)` reads forward from `offset` (the D6 runtime). `text` holds whole lines only: a trailing partial line is dropped and re-read next time; `end` is the offset after the last whole line.
- `AppServices` implements it over `EnvClient::list_processes` (for `log_end_offset` and state) and one `EnvClient::attach_output` call whose backlog it takes and whose subscription it then drops. The request runs on the services runtime, never on the actor or paint thread. Failures map to the existing `ServiceError` variants; no new error type.
- It applies to jobs whose owner is a `NamedProcess`: shell jobs and named `process` jobs. Subagent jobs get no text source at all (upstream's `lineStage` rule, `upstream:packages/coding-agent/src/job-widgets/runtime.ts:313-320`).
- Dispatcher-detached calls read the call's `<result>` text from the DOM instead (D5). No new DOM prop and no new journal entry: the feed is a read.

**D4. A model-free job view in `/hub`.** A new module `crates/chat/src/job_widgets/` holds `JobWidgetView` (title, stage, state `fallback | live | authoring`, blocks, updated-at), ported from `upstream:packages/tui/src/tools/job-widget.ts:128-139`.

- The fallback view: title is the job label; stage is the newest non-empty whole line, decomposed once with `xutf` where it enters and truncated with the existing truncation helpers; one stat block of output rate in bytes per second over a 60 s window, computed from successive `log_end` offsets, and "quiet for Ns" once the log stops growing. Bytes, not lines, because offsets give an exact rate without reading the whole log; this is a recorded divergence from upstream's `lines/s`.
- Surfaces: the roster line shows the stage in place of the kind text for a running shell or process job (`crates/chat/src/overlays/hub.rs:743`); the inspector shows the view for the selected job.
- Cadence: while `/hub` is open the panel asks for a wake every 1 s through `next_wake` and polls at most one `job_output` request per job from `tick`; nothing is requested while `/hub` is closed. A widget change marks only the overlay dirty; the transcript is not repainted (`/hub` is a `Panel` whose own frame the host composites at its anchor, `crates/chat/src/overlays/mod.rs:276-284`, `crates/chat/src/overlays/mod.rs:95-107`). The test `widget_tick_repaints_only_overlay_rows` proves it.
- TSP: `/hub` is an overlay, so it is cell-rendered while a surface is live (ADR 0041 decision 10). No TSP node is sent for it.

**D5. Dispatcher-detached calls.** For a `<job>` with a `call` prop and no `NamedProcess` owner, the view reads the call element's `<result>` text: the newest whole line and a byte rate from the text's growth. Once the text stops growing (the inline limit was reached, or the tool is silent) the view says "quiet for Ns" and keeps the last retained line, marked as retained. It never claims a live tail it cannot see. A live tail past the inline limit is open question O7.

**D6. A sparkline component and the `<spark>` tag, in core.**

- Ramp: `Charset::spark_ramp(self) -> &'static [&'static str; 8]` in `crates/tui/src/context.rs`, beside `progress()`. `Unicode` and `NerdFont`: `▁ ▂ ▃ ▄ ▅ ▆ ▇ █` (upstream `SPARK_LEVELS`). `Ascii`: `_ . - : = + * #`. No glyph appears in the component.
- Component: `omp_tui::components::Spark` in `crates/tui/src/components/spark.rs`. Builder `Spark::new().series(&[f32]).domain(min, max)`; `series` copies into a retained `Vec<f32>` that is cleared and refilled, so a refresh allocates nothing once warm. One row high; it draws the newest `rect.width` values, newest on the right. Mapping as upstream: `index = min(7, floor(clamp(fraction, 0, 1) × 8))`; domain default `[0, largest finite value]`; zero span draws the top level for a value above `min`, else the lowest; non-finite draws a space. Colour from the `fg` prop (theme token, CSS-style inheritance), default the `accent` token, as `Progress` paints (`crates/tui/src/components/progress.rs:100-106`).
- A pure helper `fit_spark_values(&[f32], cells, &mut Vec<f32>)` does upstream's equal-count bucket means, newest last, for runtimes that keep more points than they draw.
- Markup: `<spark min=0 max=10 fg=accent>1 4 2.5 8</spark>`. The body is whitespace-separated numbers, parsed once when the tag is built; a token that does not parse is a gap. `spark` is added to `is_catalog_tag`, to `is_leaf_tag`, and to the `build` table next to `progress` (`crates/tui/src/markup.rs:1074-1330`). It uses the existing `min`, `max` and `fg` props; no vocabulary change. Unknown props are ignored, as for every tag.
- TSP lowering (rule, for the first caller outside an overlay): a spark lowers to `chart` with `kind: "spark"`, `series` of `{value}`, `min`, `max` and `token` when `hello.draws(Kind::Chart)`, and otherwise to the component's painted `rows`. No slice in this plan sends one: every spark here is inside `/hub` (D4) or in the status band, where the glance's spark glyphs are dropped from the `seg` text (D7). The `wait` card is the first candidate caller (O8).
- First caller, same slice: the `/hub` inspector draws an activity spark of the byte rate per refresh for the selected shell or process job (the last 48 refreshes). This is an omp2 addition; upstream's fallback has no spark.

**D7. A deterministic rule runtime with built-in rule sets, and the chip glance.**

- Port the rule and field model with `regex`, bounded rings (240 points, 48 drawn), the ten aggregates and the limits of 12 rules and 4 blocks, into `crates/chat/src/job_widgets/`. It reads forward with `job_output(after: Some(end))`, so work per refresh is proportional to new text. Ship rule sets for a few common commands (`cargo test`, `cargo build`, `pytest`), selected by a deterministic match on the job's command text. A view driven by a rule set is `live`; a rule set that stops matching degrades to the D4 view.
- Chip glance: the `Jobs` chip (`crates/chat/src/status_band.rs:1994-2000`) appends the glance of the newest `live` widget: its first meter or spark figure and an 8-cell spark of the newest values. Fallback views never change the chip, as upstream. The glance is formatted on state change and cached; paint re-slices it. On TSP the chip is text inside the `status` node: the figure goes into its `seg`, the spark glyphs are left out.
- Blocks in this slice: spark (via D6), meter (via `Progress`), stat and text. Bars and grid follow only with S5 or a later slice.

**D8. Model-written specs are optional and last.** A forced-call schema, validation against the samples before use, redacted samples, at most 2 requests in flight, a start only after 15 s, gated by `sv_job_widgets_author` (default off). Alias the role to `smol` rather than adding a `widget` role. The request runs in the driver, beside other judge users, never in `omp-chat`.

**D9. Auto-graph is deferred.** If wanted later: first a local block-bar chart under a table with one numeric column and no model, behind `cl_table_charts`; image charts only through a rasterizer callback the host supplies so `omp-tui` stays free of `resvg`; judge refinement last.

## Rejected options

| Option | Why not |
|---|---|
| Port the full pipeline at once | Two to three weeks and a model-authored regex feature before anyone knows the surface is wanted. |
| Built-in rules only, forever | Deterministic but limited to commands someone wrote rules for; acceptable as D7, not as the end state. |
| Port auto-graph with SVG rasterization now | Weeks of work and a new `resvg` edge from `omp-tui` for a feature that needs a graphics terminal. |
| Add a `widget` model role | Changes the public role vocabulary; aliasing `smol` avoids that. |
| Read only the DOM `<result>` stream | Shell jobs detach into envd and never stream into the DOM; dispatcher-detached calls stop growing at the inline limit (`crates/envd/src/tool_shell.rs:373-381`, `crates/agent/src/dispatch.rs:1048-1085`). |
| Publish a throttled tail as a new `<job>` prop | Journals a patch per job per second and changes the `<job>` schema, which the additive rule forbids without the owner (O7). |
| A chip hover or detail surface | The chip has no hover or detail surface today (`crates/chat/src/status_band.rs:1994-2000`); building one is a separate design. `/hub` already has an inspector. |
| Ship the sparkline first with no caller | Inert until spark blocks exist; replaced by D6's activity spark in the same slice. |

## Slices

Each slice is one PR; tests are written first and not edited to pass. Every slice leaves `just lint`, `just test` and `just e2e-p7` green. "Gate off" means `cl_job_widgets 0`, the default: the existing `/hub`, status-band and P7 tests pass unchanged.

**S1. Output feed and model-free `/hub` view for shell and process jobs.**
- Scope: `cl_job_widgets` in `crates/chat/src/settings.rs`; `Services::job_output`, `JobOutputRequest`, `JobOutput` in `crates/chat/src/overlays/services.rs`; `AppServices::job_output` in `crates/app/src/chat_services/mod.rs`; `crates/chat/src/job_widgets/` (view model, fallback runtime); `/hub` roster line and inspector. Ports `upstream:packages/coding-agent/src/job-widgets/runtime.ts:639-708` (fallback view), `upstream:packages/tui/src/tools/job-widget.ts:25-139` (view types).
- Unit (`crates/chat/src/job_widgets/`): `newest_whole_line_wins_and_partial_tail_is_reread`, `rate_is_bytes_per_second_over_sixty_seconds_of_offsets`, `quiet_marker_appears_when_log_end_stops_moving`, `ansi_in_output_is_decomposed_once_at_the_feed`, `subagent_job_gets_no_text_source`.
- Crate integration (`crates/chat/tests/`, fake `Services`): `hub_roster_shows_newest_line_when_gate_on`, `hub_is_unchanged_when_gate_off`, `unavailable_feed_shows_status_only`, `at_most_one_output_request_in_flight_per_job`, `no_output_request_while_hub_is_closed`, `widget_tick_repaints_only_overlay_rows`.
- App seam (`crates/app/tests/`, real `omp-envd` named process): `job_output_tail_returns_whole_lines_before_log_end`, `job_output_after_offset_reads_forward`, `unknown_process_maps_to_failed`.
- Real PTY (`crates/e2e/tests/p7_tui.rs`): `chat_tui_hub_shows_live_job_output_through_resize_and_clean_quit`. Config `cl_job_widgets 1`; the fake provider calls `bash` with `async: true` on a loop printing `tick N` every 200 ms; keys `/hub` Enter; wait for the roster to show `tick a` then a larger `tick b`; resize the PTY and wait for the repaint at the new width; `C-c C-c`; `assert_restored`.
- Acceptance: gate on, a running shell job's newest line shows in `/hub` within two refreshes and updates while the overlay is open; gate off, nothing changes.
- Gate: `cl_job_widgets`, default off. Depends on: none. Verify: `just test-pkg omp-chat`, `just test-pkg omp-app`, `just e2e-p7`.

**S2. Dispatcher-detached calls from the DOM.**
- Scope: D5 in `crates/chat/src/job_widgets/`.
- Unit: `detached_call_shows_newest_inline_line`, `call_text_that_stops_growing_shows_retained_line_and_quiet_marker`, `rate_drops_to_zero_instead_of_freezing`.
- Real PTY: `chat_tui_hub_shows_detached_call_output_through_resize_and_clean_quit`, a fake-provider tool call that streams output past the 30 s blocking limit (the same detach path `chat_tui_drives_real_pty_tools_interrupt_resize_and_clean_quit` already exercises, `crates/e2e/tests/p7_tui.rs:102-108`); assert the line in `/hub`, resize, clean quit.
- Acceptance: gate on, a detached call shows its newest retained line and an honest quiet marker; gate off unchanged.
- Gate: `cl_job_widgets`. Depends on: S1. Verify: `just test-pkg omp-chat`, `just e2e-p7`.

**S3. Sparkline, `<spark>` tag and the activity spark.**
- Scope: D6: `Charset::spark_ramp`, `crates/tui/src/components/spark.rs`, `fit_spark_values`, the three `markup.rs` registrations; first caller the `/hub` inspector's activity spark. Ports `upstream:packages/tui/src/components/sparkline.ts:6-66`.
- Unit (`crates/tui`): `spark_frames_in_unicode_nerdfont_and_ascii` (golden rows for one fixed series in each charset), `all_zero_series_draws_the_lowest_level`, `constant_positive_series_draws_the_top_level`, `values_above_max_clamp_to_the_top_level`, `non_finite_values_draw_a_gap`, `series_wider_than_rect_keeps_the_newest_values`, `fit_spark_values_takes_bucket_means_newest_last`, `refresh_reuses_the_series_buffer`; markup: `spark_tag_parses_its_body_once`, `bad_spark_token_is_a_gap_not_a_document_failure`.
- Crate integration (`crates/chat/tests/`): `inspector_draws_activity_spark_when_gate_on`.
- Real PTY: extend the S1 case: with the job selected, the inspector row holds characters from the Unicode ramp (P7 pins `Unicode`, `crates/e2e/tests/p7_tui.rs:76-83`) and still does after the resize; clean quit.
- Acceptance: the spark draws in all three charsets with no hardcoded glyph or colour and has a wired caller when it lands.
- Gate: the component is core and ungated; its only caller is behind `cl_job_widgets`. Depends on: S1. Verify: `just test-pkg omp-tui`, `just test-pkg omp-chat`, `just e2e-p7`.

**S4. Rule runtime, built-in rule sets, chip glance.**
- Scope: D7. Ports `upstream:packages/coding-agent/src/job-widgets/spec.ts` (rule and field model, limits), `upstream:packages/coding-agent/src/job-widgets/runtime.ts` (aggregates, rings), `upstream:packages/tui/src/tools/job-widget.ts:203-279` (glance).
- Unit: `recorded_output_of_each_supported_command_replays_to_the_expected_view` (fixtures under `crates/chat/tests/fixtures/job_widgets/`), `work_per_refresh_is_proportional_to_new_text` (append 1 MB in small chunks; assert bytes scanned equals bytes appended), `rule_that_stops_matching_degrades_to_the_s1_view`, `percentage_meter_accepts_total_100`, `limits_reject_a_thirteenth_rule_and_a_fifth_block`.
- Crate integration: `chip_shows_glance_of_newest_live_widget`, `chip_is_unchanged_for_fallback_views`, `chip_glance_refresh_repaints_only_the_band`, `tsp_status_seg_carries_the_figure_without_spark_glyphs`.
- Real PTY: `chat_tui_chip_shows_rule_driven_glance_through_resize_and_clean_quit`: the fake provider runs `bash` `async` printing recorded `cargo test` lines; the band shows the passed count and spark; resize; clean quit.
- Acceptance: gate on, a supported command gets a live widget in `/hub` and a glance on the chip; unsupported commands keep the S1 view; gate off unchanged.
- Gate: `cl_job_widgets`. Depends on: S1, S3. Verify: `just test-pkg omp-chat`, `just e2e-p7`.

**S5. Model-written specs (only if wanted).**
- Scope: D8, author in the driver.
- Tests: `fake_author_spec_validates_and_drives_the_view`, `invalid_spec_is_rejected_without_effect`, `secrets_in_samples_are_redacted_before_the_request`, `at_most_two_requests_in_flight`, `no_request_before_15s`.
- Real PTY: `chat_tui_authored_widget_drives_hub_through_resize_and_clean_quit`, the fake provider answering the forced `respond` call.
- Gate: `sv_job_widgets_author` and `cl_job_widgets`, both default off. Depends on: S4.

**S6 (deferred). Local text chart under tables.** Hook `render_table` (`crates/tui/src/markdown/table.rs:197`).
- Tests: `no_numeric_column_gets_no_chart`, `single_row_gets_no_chart`, `ascii_charset_still_renders`.
- Real PTY: a fake assistant answer with a numeric table shows the chart rows; resize; clean quit.
- Gate: `cl_table_charts`, default off. Depends on: S3.

## Upstream origin and license

A port draws from `upstream:packages/coding-agent/src/job-widgets/`, `upstream:packages/tui/src/tools/job-widget.ts`, `upstream:packages/tui/src/components/sparkline.ts` and, if auto-graph is ever built, `upstream:packages/tui/src/charts/` and `upstream:packages/coding-agent/src/auto-graph/planner.ts`. TypeScript, rewritten in Rust. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Keep the notice for copied prompt text such as the `auto-graph.json` rubric.

## Consequences

- Shell and process jobs show what they are doing without a model, in `/hub`. Series and meters arrive with S4; the chip changes only for rule-driven widgets.
- `omp-tui` gains a reusable `Spark` component and `<spark>` tag any caller can use.
- An open `/hub` costs one bounded envd read per running job per second; a closed one costs nothing.
- Dispatcher-detached calls show only their retained head past 64 KiB until O7 is decided.
- Auto-graph users on upstream lose that feature until S6 or later.

## Open questions for the owner

- **O1.** Render budget for a 1 s refresh. Recommendation: only `/hub` and the status band refresh, and only while visible; resolved by D4 and D7 unless the owner wants more surfaces.
- **O2.** A `widget` role or an alias of `smol`. Recommendation: the alias.
- **O3.** Journal the views or recompute on resume? Recommendation: recompute from the envd log and the DOM; do not journal. Upstream persists the final view with the `wait` result (`upstream:packages/tui/src/tools/job-widget.ts:122-127`); omp2 does not need that until the `wait` card shows widgets (O8).
- **O4.** Live output for named `process` jobs. Resolved: they are envd named processes and use the D3 feed (`crates/driver/src/subagent/hub.rs:800-935`).
- **O5.** Where the runtime lives. Recommendation: `crates/chat/src/job_widgets/`; no new crate; `omp-tui` stays free of job knowledge.
- **O6.** Should auto-graph ever ship? Recommendation: decide after S4 usage data.
- **O7.** A live tail of a dispatcher-detached call past the inline limit needs either a new `<job>` or `<result>` prop (a schema change) or a read of the open spill stage through a new service. Recommendation: leave it out; D5's honest quiet marker is enough until someone asks.
- **O8.** Live widgets in the `wait` card, which today renders only settled snapshots (`crates/chat/src/cards/hub.rs:437-480`). That is the first caller that would send a `chart` node over TSP (D6 lowering rule) and that would need O3's persisted view. Recommendation: a later slice after S4.

## What the review changed

### Round 1 (2026-11-13; findings as recorded in `docs/adr/STATUS.md` row 0066)

- **No real-PTY proof per slice.** Confirmed: `AGENTS.md:566`, `AGENTS.md:783` and `AGENTS.md:821-822` require real-PTY proof for TUI changes, and the 2026-11-13 slices named only unit and snapshot tests. Changed: every slice now names a `crates/e2e/tests/p7_tui.rs` case built on the existing harness (`crates/e2e/tests/p7_tui.rs:740-790`): drive input, resize, `C-c C-c`, `assert_restored` (`crates/e2e/tests/p7_tui.rs:975`). S3 extends S1's case; S6 has its own.
- **Sparkline ramp API and `<spark>` tag path unspecified.** Confirmed: D2 was one sentence; `markup.rs` registers tags in three places (`crates/tui/src/markup.rs:1074-1130`, `:1155-1175`, `:1277-1330`) and the vocabulary has no series prop (`crates/vocab/src/lib.rs:36-38`). Changed: D6 names `Charset::spark_ramp` beside `Charset::progress` (`crates/tui/src/context.rs:220-225`) with both ramps, the `Spark` builder, the mapping rules ported from `upstream:packages/tui/src/components/sparkline.ts:6-66`, the body-number markup form that needs no vocabulary change, theming through `fg`, and the TSP lowering rule. While checking the mapping against upstream, the old test `constant_series_draws_the_lowest_block` turned out wrong: with the default domain a constant positive series draws the top block (`upstream:packages/tui/src/components/sparkline.ts:29-43`). It is replaced by `all_zero_series_draws_the_lowest_level` and `constant_positive_series_draws_the_top_level`.
- **Output tail and chip detail surface unspecified.** Confirmed, and worse than stated. The DOM `<result>` stream holds only the head of a call's output, up to the 64 KiB inline limit (`crates/agent/src/dispatch.rs:107`, `crates/agent/src/dispatch.rs:1048-1085`), so the old test `spilled_output_still_shows_retained_tail` could not pass. Shell jobs, the common case, detach into envd and never stream into the DOM (`crates/tools/src/shell.rs:716-746`, `crates/envd/src/tool_shell.rs:373-381`). The `Jobs` chip is a count with no detail surface (`crates/chat/src/status_band.rs:1994-2000`). Changed: D3 adds a defaulted `Services::job_output` over the existing `EnvClient::attach_output` (`crates/envd/src/exec.rs:2078-2110`); D5 covers dispatcher-detached calls from the DOM with an honest quiet marker, and O7 records the tail past the inline limit; the chip-detail idea is dropped (Rejected options) and replaced by D7's glance, shown only for rule-driven widgets as upstream does (`upstream:packages/tui/src/tools/job-widget.ts:233-240`). O4 is resolved by the same trace.
- **S1 dead code until S3.** Confirmed: upstream's fallback view has no spark (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:671-693`), so the sparkline had no caller before the rule runtime. Changed: the sparkline moved to S3, after the job view, and lands with a caller, the inspector's activity spark (D6). Every slice now ships wired.
- **TSP lowering (coordinator note on ADR 0041).** Confirmed: the presenter exists (`crates/chat/src/tsp.rs:836-850`), the wire has `chart` and `meter` kinds (`crates/tui/src/tsp/wire.rs:108-109`), and overlays are cell-rendered while a surface is live (ADR 0041 decision 10). Changed: D4 says `/hub` sends no TSP node; D6 gives the `chart`-else-`rows` rule; D7 says what the chip sends inside `status`/`seg`; O8 names the first caller that would send a `chart`. The 2026-11-13 Context claim that the wire has no chart node was wrong and is removed.
- **Corrected against this tree, found while verifying.** The gate was `sv_job_widgets`, but chat presentation convars are `cl_*` in `crates/chat/src/settings.rs` (`:37`, `:407`); renamed to `cl_job_widgets` and `cl_table_charts`. `sv_job_widgets_author` stays, because the author runs in the driver (D8). The rate is bytes per second from log offsets, not upstream's lines per second (D4, recorded divergence). Area changed from `ui` to `interface`, the README vocabulary.
- **Rejected:** none of the round-1 findings.

Rounds 2 and 3: pending (fresh-context Goldfish reviewers). Not owner-approved.

## Acceptance tests

1. S1: with `cl_job_widgets 1`, a running `bash` `async` job's newest line appears in `/hub` and updates while the overlay is open, on a real PTY, through a resize and a clean quit.
2. S1: with `cl_job_widgets 0`, the existing `/hub`, status-band and P7 tests pass unchanged.
3. S1: no `job_output` request is made while `/hub` is closed, and at most one is in flight per job.
4. S2: a dispatcher-detached call shows its newest retained line and, once its text stops growing, a quiet marker instead of a frozen rate.
5. S3: the spark draws the golden rows in `Unicode`, `NerdFont` and `Ascii`, and the `/hub` inspector draws one on a real PTY.
6. S3: a `<spark>` tag with a bad token renders a gap and the rest of the document.
7. S4: recorded output of each supported command replays to the expected view, and work per refresh equals the new bytes.
8. S4: the chip shows a glance only for a rule-driven widget, and on TSP its `seg` carries the figure without spark glyphs.
9. S5: an invalid authored spec has no effect, and secrets in samples are redacted before the request.

## Status in omp

**Status: Not started.** Reusable today: `Progress` and `Charset::progress`, the charset layer, `JobRow` and the `/hub` inspector, `Services` and `Pending`, `EnvClient::attach_output`, the DOM result stream, the TSP `chart` kind, `regex`.

## References

[docs/research/tui-job-widgets.md](../research/tui-job-widgets.md); ADR 0005 (the hub); ADR 0012 (convars); ADR 0035 (errors); ADR 0041 (surfaces over TSP); ADR 0008 and 0009 (tool output streaming and bounds).
