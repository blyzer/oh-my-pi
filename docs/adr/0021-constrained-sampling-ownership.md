# 0021. Inference owns strict-schema budgets and grammar dialects

Status: accepted
Date: 2026-09-02
Area: inference

## Context

Constrained sampling was one of the first features added to omp v1's fork of pi:

```diff
+   strict?: boolean;
+   customFormat?: { syntax: "lark" | "regex"; definition: string };
+   customWireName?: string;
```

pi followed months later with LARK and strict support, exposed as an opaque structure the provider
layer passes through. Two system-wide constraints make a pass-through insufficient:

1. **Strict-schema capacity is a shared budget.** Many providers cap the number of strict schemas
   per request. Enough independently authored extensions each setting `strict: true` make the
   provider reject every request, and the user has no way to find which plugin to patch except
   binary search.
2. **Grammar dialect is provider-specific.** A LARK grammar handed to every provider is itself
   invalid on most of them; Anthropic has never parsed one, Google rejects half of JSON Schema. An
   extension cannot maintain that map because the user may route the same model through a native
   host, a proxy, or a custom provider — routing is user-controlled, not extension-controlled.

pi's answer was a large per-provider constraint document (`CONSTRAINTS.md`, 168 lines) plus schema
utilities, and it still let three independent extensions brick a request nobody could debug
(`docs/py/13-inference.md`).

## Decision

Constraint declarations are intents; the inference layer owns everything between the intent and
the wire.

1. An extension or tool declares **intent** only: strictness, an optional grammar
   (`{ syntax: lark | regex, definition }`), and a priority. It NEVER declares a wire format.
2. Inference owns, in order:
   - **capability detection** — does this route enforce strict schemas or this grammar syntax
     (catalog data, 0017; `unknown` is not "yes");
   - **the budget** — per-route ceilings on advertised tools and strict-schema slots, spent in
     priority order across every registration, stable for a given registration set so a prompt
     prefix does not churn;
   - **dialect normalization** — the schema is rewritten once per provider flavor before emission;
   - **fallback** — when capability or budget is absent, the tool ships as plain JSON Schema and
     charitable client-side decoding validates the result (0020);
   - **surfacing** — invalid output is repaired where unambiguous and otherwise returned to the
     model as a structured, retryable error.
3. Every degradation is a receipted adjustment (`Dropped`, with a typed reason such as
   `catalog.strict-schema-unsupported` or `catalog.grammar-unsupported`), never a silent change.
4. An intent that can never be satisfied (`on_unsupported=ERROR` on a device that cannot reach the
   wire) is a declaration-time error, not a runtime surprise.

## Consequences

- A user adding a tenth strict extension does not brick the harness; the lowest-priority intents
  degrade with a visible receipt.
- The same tool works through a native host, a proxy, and a custom provider without the extension
  knowing which; the dialect layer chooses.
- Prohibited: `strict`, `customFormat`, or a wire name as raw fields on a tool definition;
  provider-name checks in extensions to decide whether to send a grammar.
- Cost accepted: an intent may be honored one turn late or not at all when the budget is full; a
  stable prefix is worth more than one turn of strictness.

## Status in omp

**Status: Partially implemented.** Intents, budgets and typed `Dropped` adjustments exist; end-to-end proof across all grammar dialects is missing. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Budgeting and lowering: `ConstraintBudgetCaps { maximum_tools, maximum_strict }` and unit tests in `crates/ai/src/plan.rs`; `ConstraintDisposition` in `crates/tool/src/registry.rs`.
- Typed degradation reasons such as `catalog.strict-schema-unsupported` appear in `crates/ai/src/codec/openai_chat.rs` and `openai_responses.rs`.
- Proved in `crates/tool/tests/contracts.rs` (2026-10-22, `omp2` at `1cc90158cc`): strict and tool-count budgets are spent in priority order across core and extension registrations with typed `Dropped` receipts; each grammar dialect (Lark, Regex, Ebnf) stays native only on a route that enforces that dialect and otherwise falls back to the plain JSON Schema as `catalog.grammar-unsupported`; `on_unsupported=Error` refuses instead of dropping for a non-strict route, an out-of-subset schema, an unsupported dialect, and a lost strict or tool-count budget.
- Diverges from the decision text: the refusal in point 4 above is raised when `Registry::advertise` lowers the tool for a route, not at registration, so it is a lowering-time error rather than a declaration-time one.
- Codec emission (2026-10-23): the OpenAI Responses codec is the only one that emits a native grammar, and `custom_tool_grammars_preserve_exact_syntax_and_definition_on_wire` already pinned Regex, Lark, and Ebnf on its wire; the earlier "not checked" note missed it. Codecs with no grammar form reject a grammar tool with a typed capability error before the wire: GitLab (`workflow_codec_rejects_custom_tool_grammars`, Lark only) and Ollama (`tool_grammars_of_every_dialect_are_rejected_before_the_wire`, all three dialects, plus `strict_tool_schemas_are_rejected_and_loose_ones_encode`). Schema-only transports read `ToolInputConstraint::wire_schema()`, which returns the non-strict fallback for every dialect (`every_grammar_dialect_degrades_to_its_non_strict_fallback_on_schema_only_transports`).
- Request bodies (2026-10-23), each pinned by `a_grammar_tool_lowers_to_its_non_strict_fallback_schema` in its codec module: OpenAI Chat sends `{"type":"function","function":{"name","parameters"}}` carrying the fallback and omits `strict` entirely; Bedrock Converse sends `toolConfig.tools[].toolSpec.inputSchema.json` carrying the fallback; Anthropic Messages sends it as a `ClientTool` with `strict` unset. Gemini refuses it instead, with `CapabilityMismatch` at the encoding phase and `RetryAction::Never` (`a_grammar_tool_is_refused_before_the_wire`).
- Resolved (2026-10-24, owner): a grammar tool's fallback ships verbatim on Anthropic. `normalize_tool_schema` gained a `close_objects` flag; `plan_tool_schemas` passes `false` for grammar fallbacks (`tool.input.grammar().is_some()`), so their deliberately loose shape is preserved while every ordinary tool schema keeps the Anthropic-wide `additionalProperties: false` closing. Pinned by `a_grammar_tool_lowers_to_its_non_strict_fallback_schema`, which now asserts the open, verbatim fallback.
- Still not checked: no codec is exercised against a live provider here, and the registry normally lowers a grammar to its fallback before these codecs see it on a route that cannot enforce the dialect, so these codec paths remain a second line of defence rather than the primary one.

## References

- The Harness Playbook, "The inference" — "Strict sampling needs budgets and dialects"
- pi `packages/ai/src/utils/schema/CONSTRAINTS.md` (the pass-through approach)
- 0016, 0017, 0018, 0020, 0022
- `crates/ai/src/plan.rs`, `crates/ai/src/call.rs`, `crates/tool/src/registry.rs`,
  `docs/py/13-inference.md`
