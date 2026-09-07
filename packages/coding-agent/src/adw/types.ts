/**
 * AI developer workflow (ADW) definitions.
 *
 * A workflow is an ordered list of phases. `agent` phases spend tokens and must
 * return an envelope; `code` phases run a command and their exit code is the
 * verdict. Sequencing, retries and acceptance belong to the Rust phase engine
 * (`TaskRun` from `@oh-my-pi/pi-natives`) — this file only describes the shape
 * the user writes on disk.
 *
 * `owner` names an agent from the roster (`.omp/agents/*.md` or a bundled one),
 * never a model. A phase may pin a model, but the roster stays the source of
 * truth for who the agent is.
 */

import { type } from "@oh-my-pi/omptype";

/** Gate names the engine knows. Keep in sync with `build_gate` in crates/pi-natives/src/tasks.rs. */
export const ADW_GATES = ["artifacts_exist", "files_non_empty", "json_parses", "diff_matches_claims"] as const;

/** One member of a fusion panel, or the fuser that merges them. */
const adwSeatSchema = type({
	owner: "string",
	/** Model pattern (`provider/id[:level]` or a `@role` alias). */
	"model?": "string",
	"thinking?": "string",
	/**
	 * What to ask this seat, when the point is division of labour rather than a
	 * second opinion. Appended after the phase's shared instructions, so a seat
	 * with one is answering a narrower question — not a different phase.
	 *
	 * Left unset, every seat answers the same question, which is what makes
	 * their answers comparable. Set it and you are buying concurrency instead:
	 * three read-only seats investigating three subsystems at once, merged by
	 * one writer. Safe precisely because no seat may write.
	 */
	"prompt?": "string",
}).onUndeclaredKey("reject");

const adwPhaseSchema = type({
	name: "string",
	kind: '"agent" | "code" | "fusion"',
	/** Agent name for `agent` phases; a subsystem label (`git`, `bun`) for `code`. */
	"owner?": "string",
	"description?": "string",
	/** Post-execution checks run against the envelope's own claims. */
	"gates?": "string[]",

	/**
	 * Phases that must pass before this one runs. Execution order becomes
	 * declaration order plus whatever these force, and the sort is a pure
	 * function of the file — ties break on declaration order, so two runs of
	 * the same workflow always execute in the same sequence.
	 *
	 * Declared, never inferred: a graph a model proposes changes between runs
	 * with the same prompt, and then `resume` cannot rebuild a position in a
	 * plan that no longer exists.
	 */
	"dependsOn?": "string[]",
	/** `agent`: model pattern override (`provider/id[:level]` or a `@role` alias). */
	"model?": "string",

	/**
	 * JSON Schema the envelope's payload must satisfy — the fields beyond
	 * `status`/`summary`/`artifacts`/`notes_for_next_agent` that this phase is
	 * asked to report.
	 *
	 * ```yaml
	 * schema:
	 *   type: object
	 *   required: [approved, blocking]
	 *   properties:
	 *     approved: { type: boolean }
	 *     blocking: { type: array, items: { type: string } }
	 * ```
	 *
	 * Structure is enforced, not semantics. `type`, `required`, `properties`,
	 * `items`, `enum`, `const` and the numeric/length bounds work. Conditionals
	 * — `if`/`then`, `allOf`, `oneOf`, `not` — are **rejected at load time**,
	 * because omptype compiles them and then ignores them: a schema carrying
	 * `if: {approved: true}, then: {blocking: {maxItems: 0}}` validated
	 * `{approved: true, blocking: ["x"]}` as fine. A conditional that silently
	 * does nothing is worse than an absent one — it reads like a guarantee.
	 *
	 * So a cross-field rule ("approved implies nothing blocking") is not
	 * expressible here; put it in a `code` phase, where an exit code cannot lie.
	 *
	 * Validated by the caller, because it needs omptype — a JSON Schema
	 * validator inside the engine crate would cost it its three dependencies.
	 * The verdict is reported back and lands in the trace as a `gate_check`.
	 */
	"schema?": "unknown",
	/** `agent`: `off|minimal|low|medium|high|xhigh|max|auto`. */
	"thinking?": "string",
	/** `agent`: extra instructions appended after the request. */
	"prompt?": "string",
	/** `code`: shell command; exit code 0 is the pass. */
	"command?": "string",
	/** `code`: per-command timeout in ms. Defaults to 10 minutes. */
	"timeoutMs?": "number",
	/**
	 * `code`: what a failure does. `retry` (the default) re-runs the command,
	 * which is right for a flaky step and useless for a deterministic one — a
	 * red test suite re-run is red again, so it burns the budget while the
	 * agent that wrote the code never learns it broke.
	 *
	 * `correct` sends the failure back to the nearest preceding `agent` or
	 * `fusion` phase as a correction in that agent's own session. The attempt
	 * is charged to that phase, which is what makes the loop finite.
	 */
	"onFail?": '"retry" | "correct"',
	/**
	 * `fusion`: two or more seats that answer the same prompt independently and
	 * read-only. Different models is the point — one model's blind spot is
	 * another's obvious answer.
	 */
	"panel?": adwSeatSchema.array(),
	/** `fusion`: the only seat allowed to write. It merges the panel and owns the envelope. */
	"fuser?": adwSeatSchema,
}).onUndeclaredKey("reject");

export const adwWorkflowSchema = type({
	name: "string",
	"description?": "string",
	/** Attempts per phase before the run halts. Defaults to 3, minimum 1. */
	"maxAttempts?": "number",
	/**
	 * Run every phase against an isolated copy of the repo and apply the result
	 * only if the run is accepted. Without it a rejected workflow leaves its
	 * half-finished edits in the working tree with no undo.
	 */
	"isolation?": "boolean",
	/**
	 * Globs `diff_matches_claims` treats as always accounted for, for paths a
	 * build legitimately rewrites without any phase claiming them (`bun.lock`,
	 * `**​/*.generated.ts`).
	 *
	 * Empty by default and deliberately so: a wide default makes the gate noisy,
	 * and an operator who cannot tell which changes it forgives stops trusting
	 * it. A malformed pattern fails the run at construction, naming itself.
	 */
	"undeclaredIgnore?": "string[]",
	phases: adwPhaseSchema.array(),
})
	// A misspelled key is not a harmless no-op: `isolaton: true` would run the
	// whole workflow against the real checkout while the operator believes it
	// is sandboxed. Every unknown key fails the file instead.
	.onUndeclaredKey("reject");

export type AdwSeatConfig = typeof adwSeatSchema.infer;
export type AdwPhaseConfig = typeof adwPhaseSchema.infer;
export type AdwWorkflowConfig = typeof adwWorkflowSchema.infer;

/** A workflow plus where it was loaded from, for error messages. */
export interface DiscoveredWorkflow {
	workflow: AdwWorkflowConfig;
	path: string;
	level: "user" | "project";
}

export interface AdwPhaseProgress {
	phase: string;
	owner: string;
	kind: AdwPhaseConfig["kind"];
	attempt: number;
	/** Set once the phase settles. */
	outcome?: "advanced" | "retry" | "aborted";
	/** Why it was rejected, when `outcome` is `retry` or `aborted`. */
	violations?: string[];
}
