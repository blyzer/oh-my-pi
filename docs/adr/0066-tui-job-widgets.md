# 0066. Job widgets: a model-free live job row first, then a deterministic rule runtime; auto-graph charts deferred

Status: proposed
Date: 2026-11-13
Area: ui

Research: [docs/research/tui-job-widgets.md](../research/tui-job-widgets.md) (Gap 12 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- Job widgets: a widget is data, not code. A cheap model writes a declarative spec once per job; a deterministic runtime applies it to each new output line; renderers draw the view (`upstream:packages/tui/src/tools/job-widget.ts:1-13`). Surfaces are the `wait` card, the HUD jobs pill and its hover card, and the jobs sheet; the switch is `async.widgets`, default on (`upstream:docs/settings.md:654`).
- A spec has up to 12 rules and 4 blocks (`upstream:packages/coding-agent/src/job-widgets/spec.ts:157-160`). A rule is a regex line matcher with named groups feeding fields, reduced by one of ten aggregates (`upstream:packages/coding-agent/src/job-widgets/spec.ts:14`, `upstream:packages/coding-agent/src/job-widgets/spec.ts:47-62`). Blocks are spark, bars, meter, stat, grid and text. Validation requires every regex to compile, every read field to be fed, and the rules to match the samples the author saw (`upstream:packages/coding-agent/src/job-widgets/spec.ts:1-7`).
- The runtime uses no model past the spec and does work proportional to new text with bounded rings (`upstream:packages/coding-agent/src/job-widgets/runtime.ts:1-8`). The registry refreshes at 1 s while watched, allows a spec request only after 15 s and at most 2 in flight, and re-authors on drift (`upstream:packages/coding-agent/src/job-widgets/registry.ts:17-45`). With no spec the view falls back to the last line and rates (`upstream:packages/tui/src/tools/job-widget.ts:25-32`).
- Upstream fixed two leaks worth remembering: widgets once showed text fetched by tools instead of the job's own output, and percentage meters needed special handling (`upstream:packages/coding-agent/CHANGELOG.md:829`, `upstream:packages/coding-agent/CHANGELOG.md:1154`).
- Auto-graph: numeric tables in answers get a chart on graphics terminals, planned locally and optionally refined by the judge (`upstream:docs/settings.md:917`, `upstream:packages/tui/src/charts/chart-plan.ts:1-8`). About 12,000 lines of chart code plus a 16.5 KB rubric.

### What omp2 has

- `omp-tui` has `Progress`, `Table`, `Img`, `Scene` and others, a charset layer (`Unicode`, `NerdFont`, `Ascii`) and a rule against hardcoded colours and glyphs (`crates/tui/src/context.rs:41-49`, `AGENTS.md:460-462`). No sparkline, no chart; the TSP wire has no chart node (`crates/tui/README.md:3-14`).
- Jobs live in the session DOM under `<meta><jobs>`; the `/hub` overlay reads each row into `JobRow` (`crates/agent/src/jobs.rs:1-6`, `crates/chat/src/overlays/hub.rs:92-113`). The status band shows a `Jobs` count chip and the `wait` card lists job snapshots (`crates/chat/src/status_band.rs:1994-1997`, `crates/chat/src/cards/hub.rs:437-480`).
- A running call's output frames append to its `<result>` text stream, bounded, with overflow spilled to a content-addressed stage (`crates/agent/src/dispatch.rs:944-967`). Live output for named `process` jobs was not traced (UNVERIFIED).
- `regex`, `resvg` and `image` are workspace dependencies; `omp-tui` does not use `resvg` (`Cargo.toml:215`, `Cargo.toml:217`, `crates/tui/Cargo.toml:28-36`). Built-in model roles include `smol`, `tiny`, `judge`, `memory`, no `widget` (`crates/catalog/src/selection.rs:175-178`).

### Rules that bind

- Deterministic rules first, a model only where rules cannot.
- Additive only; the role vocabulary is a public contract (`crates/catalog/src/selection.rs:172-178`).
- Theme and charset rules apply to every new component; secrets must not reach a model.

## Decision

**D1. Stage the work.** The cheap, model-free parts ship first; a model-written spec ships last and only if the surface proves wanted.

**D2. A sparkline component.** `<spark>` takes numbers and a width; its glyph ramp comes from the charset layer (eight blocks in Unicode and NerdFont, an ASCII ramp otherwise) and its colour from a theme tone. No hardcoded glyph or colour in the component.

**D3. A model-free job row.** A `JobWidgetView` with title, last output line and a rate, drawn in the `/hub` job row and the `Jobs` chip's detail from the DOM result stream. Refreshes must not repaint the transcript.

**D4. A deterministic rule runtime with built-in rule sets.** Port the rule and field model with `regex`, bounded rings, the ten aggregates and the limits of 12 rules and 4 blocks. Ship rule sets for a few common commands. A widget reads only the job's own output, never tool-result text a job fetched.

**D5. Model-written specs are optional and last.** A forced-call schema, validation against the samples before use, redacted samples, at most 2 requests in flight, a start only after 15 s. Alias the role to `smol` rather than adding a `widget` role.

**D6. Auto-graph is deferred.** If wanted later: first a local block-bar chart under a table with one numeric column and no model; image charts only through a rasterizer callback the host supplies so `omp-tui` stays free of `resvg`; judge refinement last.

**D7. Gates.** Default-off convar `sv_job_widgets` for D3 and D4; `sv_job_widgets_author` for D5; `sv_table_charts` for D6.

## Rejected options

- **Port the full pipeline at once.** Two to three weeks and a model-authored regex feature before anyone knows the surface is wanted.
- **Built-in rules only, forever.** Deterministic but limited to commands someone wrote rules for; acceptable as D4, not as the end state.
- **Port auto-graph with SVG rasterization now.** Weeks of work and a new `resvg` edge from `omp-tui` for a feature that needs a graphics terminal.
- **Add a `widget` model role.** It changes the public role vocabulary; aliasing `smol` avoids that.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**S1. Sparkline.** Tests: golden frames in all three charsets, `constant_series_draws_the_lowest_block`, `values_above_domain_clamp_to_full_block`, `narrow_width_keeps_the_newest_values`. No gate; inert without a caller.

**S2. Model-free job row.**
- Tests, beside `crates/chat/src/cards/hub.rs`: `running_tool_job_shows_newest_output_line_within_one_tick`, `job_without_output_shows_status`, `spilled_output_still_shows_retained_tail`, `refresh_does_not_repaint_the_transcript`.
- Gate: `sv_job_widgets` default off.

**S3. Rule runtime and built-in rule sets.**
- Tests: `recorded_output_of_each_supported_command_replays_to_the_expected_view`, `work_per_refresh_is_proportional_to_new_text` (append 1 MB in small chunks and bound total time), `rule_that_stops_matching_degrades_to_the_slice_2_view`, `widget_never_reads_tool_result_text`, `percentage_meter_accepts_total_100`.
- Gate: `sv_job_widgets`.

**S4. Model-written specs (only if wanted).**
- Tests: `fake_author_spec_validates_and_drives_the_view`, `invalid_spec_is_rejected_without_effect`, `secrets_in_samples_are_redacted_before_the_request`, `at_most_two_requests_in_flight`, `no_request_before_15s`.
- Gate: `sv_job_widgets_author`.

**S5 (deferred). Local text chart under tables.** Hook `render_table` (`crates/tui/src/markdown/table.rs:197`).
- Tests: `no_numeric_column_gets_no_chart`, `single_row_gets_no_chart`, `ascii_charset_still_renders`.
- Gate: `sv_table_charts` default off.

## Upstream origin and license

A port draws from `upstream:packages/coding-agent/src/job-widgets/`, `upstream:packages/tui/src/tools/job-widget.ts`, `upstream:packages/tui/src/components/sparkline.ts` and, if auto-graph is ever built, `upstream:packages/tui/src/charts/` and `upstream:packages/coding-agent/src/auto-graph/planner.ts`. TypeScript, rewritten in Rust. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`). Keep the notice for copied prompt text such as the `auto-graph.json` rubric.

## Consequences

- Jobs show what they are doing without a model. Series and meters arrive with S3.
- A refresh timer over many jobs adds render work; it must live in a status or hover surface.
- Auto-graph users on upstream lose that feature until S5 or later.

## Open questions for the owner

- **O1.** Render budget for a 1 s refresh. Recommendation: draw only in status or hover surfaces and let the retained `Ui` diff them.
- **O2.** A `widget` role or an alias of `smol`. Recommendation: the alias.
- **O3.** Journal the views or recompute on resume? Recommendation: recompute from retained output; do not journal.
- **O4.** Live output for named `process` jobs. Recommendation: trace it in S2 and say what is missing.
- **O5.** Where the runtime lives. Recommendation: `crates/chat` or a small new crate; keep `omp-tui` free of job knowledge.
- **O6.** Should auto-graph ever ship? Recommendation: decide after S3 usage data.

## Status in omp

**Status: Not started.** Reusable today: `Progress`, the charset layer, `JobRow`, the DOM result stream, `regex`.

## References

[docs/research/tui-job-widgets.md](../research/tui-job-widgets.md); ADR 0005 (the hub); ADR 0041 (surfaces over TSP); ADR 0012 (convars).

