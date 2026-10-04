# 0038. Stream rules are a Director over a generic stream-watch hook

Status: accepted
Date: 2026-09-29
Area: control-plane

## Context

A **stream rule** is a rule document whose frontmatter `condition` (regex) and `scope` are matched
against the model's output while it streams. On a match the harness stops the response, injects the
rule body, and samples again, so the model corrects itself before the violating output takes
effect. omp v1 called this TTSR ("time-traveling stream rules"). omp2 parses the frontmatter but has
no engine. The owner confirmed that the feature was always meant as a **generic** mechanism. It must
not become the "hardcoded per-feature outcome tracking (TTSR-style)" that `AGENTS.md` prohibits in
the agent loop, which is a generic hook surface. Regimes were removed and Directors replaced them
(0015).

**v1 behaviour** (read-only from `origin/main`: `docs/ttsr-injection-lifecycle.md`,
`docs/rulebook-matching-pipeline.md`, `packages/coding-agent/src/export/{ttsr.ts,ttsr-settings.ts}`,
`src/session/ttsr-coordinator.ts`, `src/capability/rule.ts`, `src/prompts/system/ttsr-*.md`):

| Aspect | v1 |
| --- | --- |
| Registration | A rule with `condition`, `astCondition`, or `question` goes to the TTSR bucket. That bucket wins over always-apply and rulebook, so the rule leaves the system prompt but stays readable at `rule://<name>`. Invalid regexes are skipped with a warning. `agents` globs and `ttsr.disabledRules` filter first. |
| Scope tokens | `text`, `thinking`, `tool`/`toolcall`, `tool:<name>`, `tool:<name>(<glob>)`, as a YAML list or a comma string. The default when `scope` is absent is text + every tool, **not** thinking. `globs` is a global path gate: at least one candidate path must match. |
| Condition quirks | A leading `(?i)`/`(?m)`/`(?s)` is translated to JS flags. A condition that looks like a file glob becomes `tool:edit(<glob>)`+`tool:write(<glob>)` with `.*`. The legacy key `ttsr_trigger` is accepted. Lookahead-conjunction patterns (`(?=[\s\S]*a)(?=[\s\S]*b)`) get a sticky-flag optimization. |
| Buffering | One string buffer per stream key: `text`, `thinking`, or a tool-call id. Every delta is appended, then **every** rule regex re-tests the **whole** buffer (O(n²) per response). Buffers reset at turn start, message start, and retry. For edit/write, the tool supplies a reconstructed "matcher digest" of added lines per file (`matcherEntries`), which replaces the raw JSON. Other tools match raw argument JSON. |
| Interrupt modes | `ttsr.interruptMode` (`always` \| `prose-only` \| `tool-only` \| `never`), overridable per rule with `interruptMode`. |
| On interrupt | `agent.abort()` runs synchronously. A tool match scopes the abort reason to that call id, and siblings get "TTSR interrupt on another tool call". `ttsr_triggered` is emitted fire-and-forget. A retry is scheduled after **50 ms**, guarded by a retry token, the prompt generation, and the target message identity. With `contextMode: discard` (the default) the partial assistant message is dropped from in-memory context; `keep` retains it. The rule body is appended as a hidden `custom_message` (`customType: "ttsr-injection"`) rendered from `ttsr-interrupt.md` (`<system-interrupt reason="rule_violation" rule=… path=…>`), a `ttsr_injection` entry is persisted, and `agent.continue()` resamples. |
| Non-interrupting | A tool-source match prepends a `<system-reminder reason="rule_violation">` text block **into the tool result content**. A prose match queues a hidden follow-up message after the response and continues. If the response aborts or errors, the pending buckets are dropped, but the in-memory "already injected" mark is **not** rolled back. |
| Repeat | `ttsr.repeatMode` is `once` (the default: a rule fires once per session) or `after-gap` (`ttsr.repeatGap`, default 10, counted in completed agent turns, one per assistant response). Injected names are persisted and restored on resume, but restored ages restart at zero. There is no per-turn retry cap: `once` is the only loop guard. |
| Resume | Context rebuild drops every `aborted`/`error` assistant message and its paired tool results. After a reload, `keep` therefore behaves like `discard`: the live context and the resumed context differ. |
| Surfaces | `ttsr_triggered { rules }` is sent to extensions, hooks, and custom tools. The TUI shows a `TtsrNotificationComponent` and hides the aborted stop reason. The CLI has `omp ttsr list\|test\|scan`. Settings: `ttsr.{enabled, judge, contextMode, interruptMode, repeatMode, repeatGap, builtinRules, disabledRules}`. |
| Beyond regex | `astCondition` (ast-grep on edit/write snapshots, checked at `beforeToolCall`) and `question` (a judge model role asked about the completed output, delivered as an aside warning). Neither interrupts a stream mid-flight. |

**omp2 today (audit, `origin/omp2` at `4c77afe7a5`):**

- **Rule parsing.** `crates/driver/src/discovery/rules.rs` parses `condition` and `scope`
  (`Rule::condition`/`Rule::scope`, lines 230–233, `load_rule` at 846). Nothing consumes them.
  `ActiveRules::prompt_facts` (547) still lists a described rule that has a `condition` in the
  rulebook. `interruptMode`, `astCondition`, `question`, and `ttsr_trigger` are not parsed.
- **Directors.** The `Director` trait (`crates/agent/src/director.rs:417`) has `before_inference`,
  `prepare_inference`, `before_yield`, `observe_turn`, `after_settled_turn`, and
  `on_yield`/`evaluate`. It has **no stream-time hook**. `Verdict` (136) is a yield disposition;
  nothing can say "abort this response, inject, resample". Constructors are `fn(&Node)`, so a
  Director that needs host data (compiled rules) has to be installed as an instance through
  `DirectorRegistry::register_extension`.
- **Loop.** `drive_inference` (`crates/agent/src/loop.rs:2219`) folds `ChatEvent::{TextDelta,
  ThinkingDelta, ToolArgumentsDelta, ToolCallReady, …}` from `crates/ai/src/event.rs`. `Fold`
  (3695) is `Ended | Cancelled | ToolScopedAbort(reason)`, and a scoped abort ends the **turn** as
  `TurnStop::Cancelled`. Mid-stream precedent: `streamed_edit_must_abort` (4023) aborts one
  irrecoverably invalid streamed edit through `Fold::ToolScopedAbort`. Tool execution is admitted
  only after the fold ends, so no side effect can run while a response streams. Loop-owned retry
  precedents re-derive their counts from the DOM instead of keeping shadow state
  (`paused_turn_continuation_count`, `EMPTY_OUTPUT_RETRY_CAP`).
- **Tool-scoped abort.** `ToolScopedAbortReason` and `Up::AbortTools`
  (`crates/agent/src/steering.rs:32,163`) exist. `crates/agent/tests/turn.rs:523` proves the
  sibling labels (the test still spells them "TTSR matched rule" / "TTSR interrupt on another tool
  call") and replay equality. No production code sends `Up::AbortTools`.
- **Transcript channels.** `<developer>` turn children project to the model as system-role items at
  the thread tail. `<notice>` is host-visible only. A call's `<diag>` children reach the model as
  trailing tool-result text (`crates/session/src/projection.rs`, 0008/0009). No prop excludes an
  assistant from projection; the only "context=excluded" is for local tool runs
  (`LOCAL_CONTEXT_PROP`, 426).
- **Python/proto vocabulary.** `HOOK_EVENT_TTSR_TRIGGERED = 58` and `TtsrTriggeredEventV1{rule,
  matched, interrupted}` are in `crates/proto/proto/omp/toolhost/v1/toolhost.proto:562,582`.
  `omp.events.TtsrTriggeredEvent` is at `crates/py/python/omp/events.py:842`, and
  `omp_rpc.on_ttsr_triggered` exists. `LifecycleEvent::encode` accepts the id
  (`crates/envd/src/exthost/dispatch.rs:772`). There is **no emitter**. `docs/py/10-telemetry.md`
  already rules that stream interception stays Rust-side, with Python getting an after-the-fact
  event.
- **Orphans.** `TtsrArgs`/`TtsrCommand`/`TtsrSourceArg` (`crates/app/src/cli.rs:762–830`) are not
  referenced by `Command`. The settings overlay has a "Rules (TTSR)" group with no convars.
- **Matcher crates.** The workspace has `regex` 1.13 (resolving `regex-automata` 0.4.18, whose
  lazy "hybrid" DFA is already compiled through `regex`'s default `perf` features), `fancy-regex`,
  `globset`, `memchr`, and `ast-grep-core`.

### Options considered

Hook placement:

- **H0 — hardcode matching in `loop.rs`** (v1's coordinator shape). Prohibited: this is the
  per-feature outcome tracking `AGENTS.md` forbids.
- **H1 — a Python `@omp.director` or `@omp.hook` callback per delta.** It would cross the process
  boundary for every token, and the per-call extension timeout is 5 s. `docs/py/10-telemetry.md`
  already rejects this shape. Rejected.
- **H2 — reuse `Up::AbortTools` from a sidecar observer.** This ends the turn as `Cancelled`, so a
  resample would need a second submission; the observer would also need its own copy of the
  stream. Rejected.
- **H3 — the inference layer (a recovery middleware in `omp-ai`).** Recovery layers own
  *pre-commit* gating (`OutputVisibility::Gated`, the repetition guards). Rule text, agent class,
  repeat state, and transcript placement are control-plane facts that `omp-ai` must not know
  (0016). Rejected.
- **H4 — a generic, synchronous stream-watch hook on `Director`, plus one generic loop outcome,
  "redirect".** The rule semantics live entirely in one built-in Director. **Chosen.**

Matching engine:

- **M0 — v1 shape: accumulate a `String` per key and re-run every regex over it on each delta.**
  O(n²), and it allocates as it grows. Rejected.
- **M1 — `fancy-regex`** (lookaround and backreferences). Backtracking, not incremental, and it
  can blow up on adversarial input. Rejected for the stream path.
- **M2 — `regex_automata::hybrid` multi-pattern lazy DFA, advanced byte by byte across deltas.**
  O(delta) per fragment with a 4-byte state per stream and no buffer. **Chosen.**

## Decision

### 1. A generic stream-watch hook on Directors

`Director` gains one optional method. `DirectorStack` calls it once per request, after
`prepare_inference`:

```rust
/// Opens a synchronous observer for one inference response. `None` (the default) costs nothing.
/// `cx` gains the effective `con` (as `MutDirectorCx` already carries it) for convar gating.
fn watch_stream(
   &self,
   dom: &Dom,
   cx: &DirectorCx<'_>,
   req: &ChatRequest,
) -> Option<Box<dyn StreamWatch>> {
   None
}

pub trait StreamWatch: Send {
   fn fragment(&mut self, fragment: StreamFragment<'_>) -> StreamVerdict;
   fn block_end(&mut self, index: u32) -> StreamVerdict;
   fn call_ready(&mut self, index: u32, call: &ToolCall) -> StreamVerdict;
}

pub struct StreamFragment<'a> { pub index: u32, pub source: StreamSource<'a>, pub bytes: &'a [u8] }
pub enum StreamSource<'a> { Text, Thinking, ToolArgs { call_id: &'a str, tool: &'a str } }

pub enum StreamVerdict {
   Pass,
   /// Record durable effects without stopping the response.
   Note(StreamEffect),
   /// Stop this response and resample within the same turn.
   Interrupt(StreamInterrupt),
}
pub struct StreamInterrupt {
   pub culprit: Option<ToolCallId>,  // labels siblings as in ToolScopedAbortReason
   pub partial: PartialOutput,       // Discard | Keep
   pub label:   Str,                 // culprit's placeholder label
   pub effect:  StreamEffect,
}
pub struct StreamEffect {
   pub updates:    Vec<StateUpdate>,         // this Director's state/* props
   pub developer:  Option<Str>,              // model-visible injection (turn tail)
   pub notice:     Option<Str>,              // host-visible <notice name=…>
   pub call_diags: Vec<(ToolCallId, Str)>,   // <diag severity=warn> on a call element
}
```

Rules:

1. `watch_stream` returns one boxed watcher per request. That box is the sanctioned cold `dyn`
   boundary: one allocation per network round trip. `fragment`/`block_end`/`call_ready` are
   synchronous, take `&mut self`, and MUST NOT allocate when they return `Pass`. When no Director
   returns a watcher, the fold pays one `is_empty()` branch per event, which is the entire cost.
2. The loop feeds every `TextDelta`, `ThinkingDelta`, and `ToolArgumentsDelta` to the watchers
   **after** the fragment is coalesced into the journal: everything received is journaled, and
   matching never reorders the transcript. `block_end` fires at each content block's end and at
   `Completed`, and `call_ready` fires at `ToolCallReady`. That point is still before execution is
   admitted, so no watcher decision races a side effect.
3. Watchers run in stack order (outermost first). The first `Interrupt` ends the fold. `Note`s from
   every watcher are kept.
4. **Redirect.** `Fold` gains `Redirect(StreamInterrupt)`. On redirect the loop:
   - drops the provider stream (request-scoped; the turn's cancellation token is **not** cancelled);
   - settles every materialized call as `Abort::Skipped`, with the culprit's label and the neutral
     sibling label (the existing `ToolScopedAbortReason` path);
   - closes the assistant with stop reason `interrupted`;
   - journals the redirect transaction (§4);
   - `continue`s `run_turn_body`. The resample counts toward `requests_started` and
     `RunControl::permits_request`.

   Top-of-loop cancellation, deadline, admission, and pause checks run before the resample.
5. **Generic cap.** Convar `ai_stream_redirect_cap` (default 3) bounds redirects per `<turn>`. The
   count is re-derived from the DOM: assistants in the turn carrying `interrupt=…`, never a counter
   in memory. Past the cap the loop downgrades `Interrupt` to `Note`, keeping its effect, and
   journals one `<notice kind=warn name=stream-redirect-cap>`.
6. The hook is **Rust-only**. `@omp.director` does not gain it (H1). The loop knows nothing about
   rules; it knows fragments, verdicts, and redirects.

### 2. The `stream_rules` Director

A built-in Director, `crates/agent/src/directors/stream_rules.rs`, family `stream-rules`, claims
**no** `Slot`, so it composes with plan, goal, vibe, and force-tool without exclusivity.

- **Installation.** `compose_kernel` (driver) compiles the session's admitted rules into an
  `Arc<StreamRuleSet>` and installs the Director instance in the registry. It rebuilds the instance
  on `resync_session_state` when the agent class or discovered rules change. The driver engages the
  Director once per session, and only when the admitted set is non-empty. The loop never engages it
  (unlike `CompactionDirector`), so `loop.rs` stays feature-free.
- **Gating.** `watch_stream` returns `None` when `ai_stream_rules_enabled` is false, when the turn
  is hidden (the compaction summarizer, auxiliary `ErasedInference` calls), or when every rule is
  exhausted by its repeat policy. Toggling the convar needs no disengage.
- **`on_yield`** returns `Pass`, except when deferred prose reminders are pending (§3): then it
  returns `Continue { reminder }`. If an inner Director consumes the yield (0015's contract), the
  pending reminders are delivered by `before_inference` as a developer aside on the next request
  (`Prepared::Rebuild`).

### 3. Rule semantics (v1 parity, with deliberate changes)

1. **Registration.** A rule is a stream rule when `condition` is non-empty and at least one pattern
   compiles. Discovery MUST drop stream rules from always-apply and rulebook prompt facts (v1's
   bucket priority). They stay readable at `rule://<name>`. Invalid patterns and unreachable scopes
   become typed discovery `Warning`s naming the rule and the pattern index; the rest of the rule set
   loads.
2. **Scope.** `text`, `thinking`, `tool`/`toolcall`, `tool:<name>`, and `tool:<name>(<glob>)`
   parse into a typed `StreamScope`. The default is text plus any tool, not thinking. `globs` is a
   global path gate. Tool names match the resolved stable tool name (`name`, not `name@rev`).
3. **Interrupt policy.** Convar `ai_stream_rules_interrupt` (`always` \| `prose-only` \|
   `tool-only` \| `never`, strum-derived) with a per-rule frontmatter override `interruptMode`
   (same values).
4. **Tool-argument surface.** The watcher decodes JSON string *values* from the raw argument bytes
   incrementally. This is a small escape-aware state machine that handles `\uXXXX` and surrogate
   pairs split across fragments. Object keys and non-string atoms are skipped, and consecutive
   values are separated by `\n`, so `^`/`$` in `(?m)` work per value. Candidate paths come from
   top-level `path`/`paths` values.

   A hit on a path-gated rule is **held** until a path is known. It resolves at the latest at
   `call_ready`, and it is dropped if no path matches. A call that arrives complete, with no deltas,
   is fed once at `call_ready`.

   Per-file "added lines only" projections (v1 `matcherEntries`) are implementation step 4.
5. **Interrupt.** The rule is recorded (§4) and the response is redirected.
   - `ai_stream_rules_context` (`discard` default \| `keep`) selects `PartialOutput`.
   - `culprit` is the matching call for tool sources. Its label is `stream rule <name>`, and
     siblings get `interrupted by a stream rule on another tool call`.
6. **Non-interrupting matches** use the structured channels, never prose spliced into data (0009):
   - a tool source becomes a `<diag severity=warn kind=stream-rule rule=<name>>` child of the call
     element whose content is the rule body; the model sees it as trailing tool-result text;
   - a text or thinking source becomes a pending reminder (a Director state update), delivered by
     `on_yield`/`before_inference` as in §2.
7. **Repeat.** `ai_stream_rules_repeat` (`once` default \| `after-gap`) and
   `ai_stream_rules_repeat_gap` (default 10). The gap is measured in **assistant responses**
   (v1's unit), counted from the DOM.
   - The fired record is the Director state `state/fired.<rule>` = the assistant ordinal at fire.
   - A match on an aborted response is recorded only if the redirect transaction commits, so
     nothing is marked without being delivered (this fixes v1's un-rolled-back mark).
   - After resume the ages are exact, because they derive from the tree (v1 reset them to zero).
8. `ai_stream_rules_disabled` (list of rule names) filters before compilation.
9. **Not ported:**
   - the legacy key `ttsr_trigger` (no aliases);
   - v1's glob-in-`condition` shorthand (open question 3);
   - lookaround and backreferences (§5);
   - `astCondition` and `question` (open question 6);
   - builtin default rules (omp2 ships none; open question 7).

### 4. Journal, replay, and rewind

A redirect is one atomic `patch@1` transaction, labelled `director.stream-interrupt`. It is
appended after the skipped-call placeholders, which the existing abort path already journals. It
contains:

- `interrupt=<director id>` on the assistant element and its stop reason `interrupted`;
- with `Discard`, `context=excluded` on that assistant. This generalizes `LOCAL_CONTEXT_PROP`.
  Projection MUST omit an excluded assistant **and** the call elements it issued, results included,
  because providers reject orphan tool results. With `Keep`, the assistant and the placeholder
  results stay in context;
- a host-visible `<notice kind=warn name=stream-rule>` with the rule name, the source, the call id,
  the match end offset, and a bounded excerpt (≤ 256 bytes). The excerpt is recovered cold at hit
  time from the journaled stream text, never from a parallel buffer;
- the model-visible `<developer>` node carrying the rendered injection, **verbatim**;
- the Director's `state/fired.<rule>` updates.

Consequences for determinism:

- Replay, resume, spectators, and rewind never re-run the matcher. The verdict, the injected text,
  and the context exclusion are journaled facts. The rule file is never re-read to reconstruct
  history.
- The live projection and the replayed projection are identical by construction. This fixes v1's
  `keep`-after-reload divergence.
- If a crash lands between the placeholders and the redirect transaction, the resumed turn has the
  existing cancelled-inference shape: nothing is marked and nothing is resampled.
- Rewinding past the transaction removes the exclusion, the injection, and the fired marks
  together (0004).

### 5. Matching engine

- **Crate.** Add `regex-automata = { version = "0.4", default-features = false, features = ["std",
  "syntax", "hybrid", "perf", "unicode"] }` to root `[workspace.dependencies]`. It is already in
  the graph through `regex`, so no new crate is added. No hasher or Unicode utility crates are
  added. `fancy-regex` is not used on this path.
- **Compilation.** It happens once per rule-set revision, never per request. The set holds one
  multi-pattern `hybrid::dfa::DFA` per source class (text, thinking) and one per tool name that has
  scoped rules plus the any-tool patterns. Per-tool DFAs are built lazily on the first call to that
  tool and kept in a `FastHashMap<Str, _>`. `MatchKind::All` and an unanchored start are used.
  Patterns use Rust `regex` syntax: inline flags such as `(?i)`/`(?m)`/`(?s)` work natively, and a
  pattern the lazy DFA rejects (look-around beyond `^ $ \A \z \b`, or backreferences) is a
  discovery warning.
- **Per-stream state.** Each stream key (text, thinking, each call id) holds a `LazyStateID`
  (4 bytes) and one `hybrid::dfa::Cache` borrowed from a pool the set owns. Caches are recycled
  across requests, and one cache is never shared between concurrent keys, because a cache clear
  invalidates the other keys' state ids. Steady state allocates nothing.
- **Cost.** One transition per byte, O(delta) per fragment, with no buffer copy and no rescan.
  - The lazy DFA reports a match one byte late; `block_end` feeds the end-of-input transition.
    End of block counts as end of input for `$`/`\z`.
  - Unicode `\b` uses the DFA's heuristic mode. A quit byte downgrades that key to one cold
    `regex::bytes::RegexSet` pass over the journaled block text at `block_end`, which can only
    produce a `Note` or an end-of-response interrupt.
  - The per-match cold path resolves pattern → rule, applies scope, path, and repeat checks, and
    renders the injection.

### 6. Injection placement and the prompt cache

- The injection is a `<developer>` turn child at the thread tail. It is NEVER written into a system
  prompt band. A band edit would change the Stable hash and cost a full cache miss for the rest of
  the session.
- Stream rules leave the rulebook at discovery time, not when they fire, so a firing never changes
  a band.
- With `Discard`, the resample request is the interrupted request plus one trailing developer item.
  With `Keep`, it is the interrupted request plus the partial assistant, its placeholders, and the
  developer item. Both are strict extensions of a prefix that was already sent.
- The envelope is one scribe template, `crates/agent/prompts/recovery/stream-rule.md`. It uses the
  `<system-interrupt reason="rule_violation" rule=… path=…>` envelope that the loop-guard redirects
  already use, with an `interrupted` slot, so the interrupt and the deferred reminder share one
  channel and one shape. The tool path uses `<diag>` (§3.6).

### 7. Subagents, cancellation, and the Python surface

- **Subagents.** Each child kernel composes its own `stream-rules` instance from the parent's
  discovered rules filtered by the child's agent class (`Rule::admits`), exactly as `rule://` does.
  Convars seed from the parent (0013). A child's redirects are journaled in the child's session and
  count against the child's request budget. The parent's own rules still watch the parent's
  streamed `task` arguments.
- **Cancellation (0011).**
  - The redirect is a loop `continue`. An `Interrupt`/`Cancel` accepted during or after the redirect
    transaction stops the turn at the next top-of-loop check, and the journaled injection simply
    stays in context.
  - There is no timer, retry token, or generation guard (v1's 50 ms window): the loop is one actor
    with one mailbox.
  - `Up::AbortTools` is unchanged and not used by this design.
- **Python.** The hook event is renamed in a clean cutover, keeping wire number 58:
  - `HOOK_EVENT_TTSR_TRIGGERED` → `HOOK_EVENT_STREAM_RULE_TRIGGERED`;
  - `TtsrTriggeredEventV1` → `StreamRuleTriggeredEventV1{context, rule, matched, interrupted,
    source, call_id}`;
  - `omp.events.TtsrTriggeredEvent` → `StreamRuleTriggeredEvent`, and the event id
    `stream_rule_triggered`;
  - `omp_rpc` `on_ttsr_triggered` → `on_stream_rule_triggered`.

  No aliases. The Director emits the event through `LifecycleHooks` **after** the transaction (or
  the `Note`) is journaled. It is observe-only (the existing `_OBSERVE` / `OnFailure.DEFER` spec)
  and never blocks the resample. Extensions contribute rules as files (the plugin `rules/`
  discovery source), not callbacks.
- **Hosts.** `KernelEvent::StreamRedirected { culprit }` is ephemeral and only wakes hosts; the
  journal carries the facts. The TUI renders the interrupted assistant and the notice from the
  props (0032). ACP and print render the notice like other notices.

### 8. Settings (convars, 0012)

All flags are `archive | session`, `ui.tab=context`, `ui.group="Rules (TTSR)"` (renamed to
"Stream Rules"). `legacy.path` maps the v1 keys on import; that is an import mapping, not an alias.

| Convar | Type / default | v1 `legacy.path` |
| --- | --- | --- |
| `ai_stream_rules_enabled` | bool, `true` | `ttsr.enabled` |
| `ai_stream_rules_interrupt` | enum, `always` | `ttsr.interruptMode` |
| `ai_stream_rules_context` | enum `discard`\|`keep`, `discard` | `ttsr.contextMode` |
| `ai_stream_rules_repeat` | enum `once`\|`after-gap`, `once` | `ttsr.repeatMode` |
| `ai_stream_rules_repeat_gap` | int (assistant responses), `10` | `ttsr.repeatGap` |
| `ai_stream_rules_disabled` | list of names, empty | `ttsr.disabledRules` |
| `ai_stream_redirect_cap` | int per turn, `3` (generic loop cap, §1.5) | none |

## Consequences

- Stream rules come back without teaching the loop about rules. The loop gains one reusable
  capability, "a Director may stop a response and resample". Any future stream-time behaviour
  (schema divergence, forbidden-token guards) uses the same hook and the same redirect.
- Matching is linear in output bytes and allocation-free in steady state, against v1's quadratic
  rescan.
- Redirects are journaled facts. Replay, rewind, resume, and spectators agree with the live run,
  and the repeat policy survives restarts exactly.
- The cached prompt prefix survives every firing.
- Costs accepted:
  - the violating fragment is journaled and was visible before the interrupt (journal honesty
    over "time travel");
  - the lazy DFA's one-byte match delay;
  - a skipped-placeholder round trip for materialized calls;
  - no Python stream-time callbacks;
  - patterns needing look-around or backreferences are rejected, not emulated;
  - generic tools match decoded argument strings, which include removed/context lines for edit
    formats until phase 4.
- Prohibited:
  - rule or feature names in `loop.rs`;
  - per-token Python or IPC callbacks;
  - accumulating per-stream `String` buffers for matching;
  - writing rule text into system prompt bands at fire time;
  - splicing reminders into `<result>` data;
  - in-memory repeat counters;
  - `ttsr_*` aliases after the rename.

## Amendment (2026-10-04)

The owner resolved the eight open questions as this record proposed (see "Open questions for the
owner"): `discard` stays the default context policy, the hook is renamed to
`stream_rule_triggered` without an alias, `scope` must be explicit, there is no lookahead
conjunction, the per-turn redirect cap is 3 and downgrades to `Note`, `astCondition` and `question`
rules are later records, omp2 ships no built-in stream rules, and interrupted-response usage is
not receipted. The decision text above already states each of these; the record is accepted.

## Status in omp

**Status: Partially implemented (plan steps 1-5 done; the test plan and two decision details are open, listed below).** Verified 2026-10-04
against `omp2` at `083b38fe7d`, plus the step 5 changes recorded here. The generic `StreamWatch`
hook and loop redirect are in `crates/agent`; the built-in Director, incremental DFA matcher,
policy convars, discovery filtering, and driver installation are also present.

Tool-specific authored-text projections use the `omp-tool` contract. Hashline `edit` projects
inserted rows, replace projects replacement text, patch and apply-patch project added lines or
created-file contents, sloppy projects rewrite candidates, and `write` projects its new content.
Each segment carries its target path when the dialect identifies one, so path-scoped rules only
match text intended for that file. Deletions, removed diff rows, and context rows are excluded.
`ast_edit` remains outside these textual edit dialects. Step 5 is implemented: the `omp rules`
CLI and the hook rename, emitter, and host surfaces (see the two "Surfaces as built" sections
below).

Verification notes. The matcher, Director and policy types live in one file,
`crates/agent/src/directors/stream_rules.rs` (not the `stream_rules/matcher.rs` split named in plan
step 2); the six convars (`ai_stream_rules_*`) are in `crates/agent/src/vars.rs` and
`AI_STREAM_REDIRECT_CAP` in `crates/ai`; driver installation is in
`crates/driver/src/headless/kernel.rs`. Coverage: unit tests in `stream_rules.rs` (matching, scope,
path gates, typed compile warnings, the probe agreeing with the live watch);
`crates/agent/tests/stream_watch.rs` drives a scripted watcher through the loop (redirect, the
trailing developer item, the excluded assistant, the notice, the `stream_rule_triggered`
observation after commit, the cap downgrading to `Note`, replay equality); `crates/driver`
covers `omp rules` composition; P7 (`chat_tui_renders_a_stream_rule_redirect_through_resize_and_clean_quit`)
renders a project rule's redirect on a real PTY. Still unproven: proptests over byte splits, the
JSON-string decoder, the allocation-free Pass path, rewind across a redirect, `Up::Interrupt`
during a redirect, the request-budget interaction, `Keep` projection, tool-culprit labels through
the loop, and the P2/P5/P6 rows of the test plan. The Director also still compiles its rules into
one `StreamRuleSet` without the pooled per-key caches §5 describes, and the notice and the hook
event carry no matched excerpt (§4).

### Surfaces as built: `omp rules`

`omp rules [list|test|scan]` (`crates/app/src/rules_cmd.rs`, presentation only) runs on
`omp_driver::rules::stream::StreamRuleInspector`, which composes what a kernel composes: the same
`ActiveRules::discover` call, the `agents:` filter (`--agent`, default `main`),
`ActiveRules::stream_patterns`, and `StreamRuleSet::compile`. Matching runs through
`StreamRuleSet::probe`, which drives the Director's own automaton, scope and path gates, and
interrupt-policy resolution. There is no second matcher.

- `list` prints every admitted rule with a `condition`: scope, conditions with the compiled count,
  globs, the effective interrupt policy (frontmatter `interruptMode` over
  `ai_stream_rules_interrupt`), whether `ai_stream_rules_disabled` names it, and every typed
  compile warning (`StreamRuleWarning`) and discovery warning.
- `test` matches a snippet, a file, or standard input as one complete block from `--source`
  (`text`, `thinking`, or `tool` with `--tool` and `--path`). It reports each rule once, with the
  line, a bounded excerpt, and whether a live session would interrupt or only note.
- `scan` has the smallest honest live meaning: each UTF-8 file under a directory is matched as
  the authored text of one `--tool` call (default `write`) whose target is the file's path
  relative to the project. It answers "would the model writing this file have tripped a rule".
  Rules scoped only to `text` or `thinking` never match a scan; non-UTF-8 files are skipped and
  counted. Scanning a session journal is not offered: replay never re-runs the matcher, and the
  journal already records every redirect and note as facts.

Compile diagnostics are now typed: `StreamRuleSet::compile` returns `CompiledStreamRules { set,
warnings }`, and kernel composition logs the same warnings. Discovery's "is this a stream rule"
test (`Rule::is_stream_rule`) now asks the streaming matcher (`condition_compiles`) instead of
`regex::Regex`, so a condition the DFA rejects (for example a Unicode `\b`) no longer drops a
rule from the prompt while also never matching. The orphan `TtsrArgs`/`TtsrCommand`/`TtsrSourceArg`
and `AgentsArgs`/`AgentsAction` clap types are deleted.

### Surfaces as built: the hook, the emitter, and hosts

- **Rename.** `HOOK_EVENT_TTSR_TRIGGERED` is `HOOK_EVENT_STREAM_RULE_TRIGGERED` (wire number 58),
  `TtsrTriggeredEventV1` is `StreamRuleTriggeredEventV1{context, rule, matched, interrupted,
  source, call_id}`, `omp.events.TtsrTriggeredEvent` is `StreamRuleTriggeredEvent` with event id
  `stream_rule_triggered`, and `omp_rpc`'s `on_ttsr_triggered` is `on_stream_rule_triggered`. No
  alias remains.
- **Emitter.** `StreamEffect` gained a generic `observation: Option<StreamObservation>` (a hook
  event id plus JSON fields). The loop publishes it through `LifecycleHooks::notify` only after the
  effect's transaction is journaled, stamping `session_id`, `turn_id`, `sequence` (the DOM
  high-water mark), and `interrupted` (whether the effect committed with a redirect, so a capped
  interrupt reports `false`). The loop still knows no rule names. The stream-rules Director attaches
  `{rule, matched, source, call_id}`, where `matched` is the condition that matched: the watcher
  keeps no text buffer, so the matched excerpt is not reported (an excerpt recovered cold from the
  journal stays open).
- **Notice.** A redirect journals `<notice kind=warn name=stream-rule>` naming the rule and the
  source (text, thinking, or the tool call), without the excerpt (same reason).
- **Hosts.** `KernelEvent::StreamRedirected` now carries `{director, label, reason}` and a new
  `KernelEvent::StreamObserved {director, event, payload}` mirrors each published observation.
  The TUI renders `<notice name=stream-*>` as a card led by the themed `rule-extension` icon; print
  JSON and RPC emit `stream_redirected` and `stream_rule_triggered` frames, print text mode writes
  the redirect to stderr; ACP sends stream notices as `agent_thought_chunk` updates, live and on
  replay. The settings group is "Stream Rules", and the convars carry their v1 `legacy.path`.

### Implementation plan (PR-sized)

1. **Generic hook + redirect** (`crates/agent`, `crates/session`).
   - `watch_stream`/`StreamWatch`/`StreamVerdict`/`StreamEffect`, and `DirectorStack::watch_stream`.
   - `Fold::Redirect` and the redirect transaction.
   - `context=excluded` for assistants in projection, dropping the issued calls with them.
   - `ai_stream_redirect_cap` with the DOM-derived count, and `KernelEvent::StreamRedirected`.

   No Director uses it yet, so behaviour is unchanged. Tests use a scripted test Director.
2. **Matcher** (`crates/agent/src/directors/stream_rules/matcher.rs`, workspace
   `regex-automata`). The pooled-cache multi-pattern DFA set, the streaming JSON-string decoder,
   scope/path types, and proptests. Pure, with no loop wiring.
3. **Director + discovery + convars** (`crates/agent`, `crates/driver`).
   - Parse `interruptMode`; bucket stream rules out of prompt facts; add compile warnings.
   - The Director with repeat state and deferred reminders; `stream-rule.md`; the convars with
     `legacy.path`.
   - Driver install/engage/resync, including subagent composition.
4. **Tool match surfaces** (`crates/tool`, `crates/tools`). An optional contract by which a tool
   projects its streaming arguments into path-aware authored-text segments; edit families and
   write implement it. The watcher prefers it over decoded strings. Implemented for hashline,
   replace, patch, apply-patch, and sloppy edit dialects.
5. **Surfaces** (`crates/proto`, `crates/py`, `crates/envd`, `crates/app`, `crates/chat`).
   - The hook rename and emitter; update `docs/py` (05/10/15) and `docs/pyx` in the same change.
   - `omp rules list|test|scan` over the same matcher, replacing the orphan `Ttsr*` clap types
     (deleted).
   - TUI/ACP/print rendering; relabel the `turn.rs` fixture strings.

### Test plan (owning seams)

- **`crates/agent/tests/stream_watch.rs`** (scripted inference and a scripted test Director):
  - without a watcher, the journal is byte-identical to today;
  - `Interrupt` on text → one redirect transaction, the next request carries the developer item as
    its last item, and the excluded assistant is absent from the projection;
  - a tool culprit → culprit and sibling labels, and with `Discard` both calls are absent from the
    projection;
  - `Keep` → the partial assistant and the placeholders are projected;
  - the cap downgrades to `Note` plus a notice, and the count survives replay;
  - `Up::Interrupt` during a redirect → `TurnStop::Cancelled` with no resample;
  - the redirect counts against `RunControl::with_request_budget`;
  - replayed `dom().snapshot()` equals live after a redirect.
- **`crates/agent/tests/directors/stream_rules.rs`:**
  - default scope ignores thinking; `thinking` and `tool:<name>` scopes;
  - a `tool:edit(*.ts)` hit held until `path` arrives, and dropped on a non-matching path;
  - a complete call without deltas matched at `call_ready`;
  - `interruptMode` override precedence;
  - `once` and `after-gap` across a resume (the ages derive from assistant ordinals);
  - a match on a response that fails before the transaction is not marked;
  - `never` → `<diag>` on the call and a deferred prose reminder via `Continue`;
  - hidden and compaction turns are not watched;
  - the Python event fires once, after commit.
- **Proptests** (the `crates/agent` matcher module; proptest is already a workspace dependency):
  - for random pattern sets, texts, and **arbitrary byte-level splits**, the incremental verdict
    after `block_end` equals `regex::bytes::RegexSet::is_match` on the whole text;
  - the first hit is reported no later than one byte after the earliest prefix that matches;
  - the JSON-string decoder yields identical output for every split of any valid JSON document,
    including split escapes and split surrogate pairs;
  - Pass-path fragments allocate nothing, checked with a counting allocator in a unit test.
- **`crates/driver`:** stream rules leave `prompt_facts` but resolve at `rule://`; an invalid
  pattern produces a warning while siblings load; `agents` filtering per child class; disabled
  names.
- **`crates/e2e`:**
  - **P5**: the Frozen/Stable/Dynamic band hashes are unchanged across a redirect, and the resample
    request's items are a strict prefix extension of the interrupted request.
  - **P6**: a crash between the placeholder settlement and the redirect transaction resumes as a
    cancelled inference with no fired mark; a crash after it replays byte-identically.
  - **P2**: a cancel row for "interrupt during redirect".
  - **P7**: a real-PTY rendering of an interrupted assistant and its notice, followed by a clean
    quit.

### Could not verify

- Hybrid-DFA details are taken from the `regex-automata` 0.4.18 API (`next_state`,
  `next_eoi_state`, `unicode_word_boundary`) and have not been exercised here: that a cache clear
  invalidates other keys' `LazyStateID`s, and how heuristic Unicode word boundaries quit.
- Whether dropping a `ChatStream` promptly aborts the upstream request on every transport, and
  whether an interrupted request's prefix was already written to the provider's prompt cache before
  the resample.
- That `ToolArgumentsDelta` fragments are always UTF-8 aligned. The loop already errors otherwise,
  and the matcher is byte-level anyway.
- Whether v1 persisted the discarded partial assistant to the session file: the in-memory drop is
  verified, the file write is not. v1 behaviour was read from docs and code on `origin/main`, not
  executed.
- Whether the removed regime design's `STREAM` "recoverable cancel" (formerly `15-regimes.md`) was
  ever implemented beyond documentation. It is cited only as prior art. `docs/py/15-directors.md`
  still marks the `ttsr_triggered` emitter as **Unknown**; this record answers that (the Director,
  after commit), and step 5 updates the chapter.
- Usage accounting for interrupted requests: today a cancelled inference writes no receipt, so the
  tokens spent on an interrupted response are not recorded.

### Open questions for the owner (resolved 2026-10-04)

The owner resolved every question as this record proposed:

1. Default `ai_stream_rules_context`: **`discard`** (the v1 default; best for the prompt cache).
2. The hook is renamed **`ttsr_triggered` → `stream_rule_triggered`** in one clean cutover, with
   no alias.
3. **An explicit `scope` is required.** v1's glob-looking `condition` shorthand is not ported.
4. **No lookahead conjunction now.** There is no `all:` list; a pattern the lazy DFA rejects stays
   a compile warning.
5. **Per-turn redirect cap 3**, downgrading the verdict to `Note` (with one `stream-redirect-cap`
   notice) when it is exceeded, never failing the turn.
6. **`astCondition` and judged `question` rules are separate, later records**, out of scope here.
7. **No built-in default rules.** omp2 ships none.
8. **Interrupted-response usage is not receipted now.** The cancelled-inference receipt contract
   that `turn.rs` asserts does not change.

## References

- 0003 (journal authority), 0004 (rewind/resume derive from the tree), 0008/0009 (element, `<diag>`
  channel), 0011 (cancellation), 0012/0013 (convars, seeding), 0015 (Directors), 0016 (intent, not
  mechanism), 0032 (presentation in the renderer), 0023 (tiny local model)
- `AGENTS.md`: Locked Deviations (control plane, prompts), Allocation and Async discipline
- `crates/agent/src/{director.rs,loop.rs,steering.rs}`, `crates/agent/src/directors/`,
  `crates/agent/tests/turn.rs`, `crates/session/src/projection.rs`,
  `crates/driver/src/discovery/rules.rs`, `crates/driver/src/headless/kernel.rs`,
  `crates/ai/src/{event.rs,recovery/repetition.rs}`, `crates/agent/prompts/recovery/`,
  `crates/proto/proto/omp/toolhost/v1/toolhost.proto`, `crates/py/python/omp/events.py`,
  `crates/py/python/omp_rpc/`, `crates/envd/src/exthost/dispatch.rs`, `crates/app/src/cli.rs`,
  `docs/py/{05-hooks.md,10-telemetry.md,15-directors.md}`
- v1 (`origin/main`): `docs/{ttsr-injection-lifecycle.md,rulebook-matching-pipeline.md}`,
  `packages/coding-agent/src/export/{ttsr.ts,ttsr-settings.ts}`,
  `packages/coding-agent/src/session/{ttsr-coordinator.ts,session-context.ts}`,
  `packages/coding-agent/src/capability/{rule.ts,rule-buckets.ts}`,
  `packages/coding-agent/src/prompts/system/ttsr-{interrupt,tool-reminder,warning}.md`
- `regex-automata` 0.4 `hybrid::dfa` (lazy DFA, incremental `next_state`)
