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
	/**
	 * `human` is the engine's `Engineer` lane: the caller answers and reports
	 * back, so the run holds rather than guessing. Reserved for what cannot
	 * be delegated reliably — irreversible, security-sensitive, legally
	 * binding, externally visible. Used for ordinary review it turns the
	 * operator back into the orchestrator the workflow exists to replace.
	 */
	kind: '"agent" | "code" | "fusion" | "human"',
	/** Agent name for `agent` phases; a subsystem label (`git`, `bun`) for `code`. */
	"owner?": "string",
	"description?": "string",
	/** Post-execution checks run against the envelope's own claims. */
	"gates?": "string[]",

	/**
	 * Phases that must pass before this one runs. Omitted: the declaration
	 * predecessor is an implicit dependency. Explicit []: independent.
	 * Ready phases dispatch in declaration order, bounded by concurrency.
	 * Inputs, corrections and revisions all traverse this same graph.
	 */
	"dependsOn?": "string[]",
	/**
	 * Phases whose accepted outputs this phase consumes, by name. Each must
	 * belong to the effective transitive dependency closure, including implicit
	 * predecessor edges. Dispatch resolves the producer's current accepted
	 * envelope and labels its version in both the prompt and trace.
	 */
	"inputs?": "string[]",
	/**
	 * `agent`/`fusion`: repo-relative globs naming what this phase's writer may
	 * change. Protection wins: a path matching both `writes` and a protected
	 * glob stays forbidden — declaring it does not authorize it. Absent:
	 * unrestricted except protected paths, allowed only with concurrency 1.
	 * Explicit [] denies all repository writes. Every concurrent agent/fuser
	 * must declare writes, including read-only reviewers.
	 */
	"writes?": "string[]",
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
	 * Uses omptype’s JSON Schema subset, not a full-conformance validator.
	 * `anyOf`/`allOf` support cross-field rules with self-contained branches;
	 * sibling constraints are not applied, and numeric/string bounds need an
	 * explicit `type` on the same node. Known unsupported constructs such as
	 * `if`/`then`/`else`, `oneOf`, and `not` are rejected at load time.
	 *
	 * Native predicates cannot be callbacks in this JSON/YAML field. The
	 * `verdict_consistent` gate supplies the built-in native review rules.
	 * Custom rules needing JavaScript can run in a `code` phase.
	 * The caller validates the whole envelope for agents and fusion fusers,
	 * reports the checks to the engine, and includes the schema in the prompt.
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
	 * `agent`/`fusion` with `verdict_consistent`: a coherent rejection revisits
	 * this earlier builder after all gates pass. Each revision grants the
	 * target one additional attempt; maxRevisions bounds this route for the run.
	 */
	"onReject?": type({ to: "string", maxRevisions: "number" }).onUndeclaredKey("reject"),
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
	/** Base attempts per phase before revision grants. Defaults to 3, minimum 1. */
	"maxAttempts?": "number",
	/**
	 * Maximum phases in flight at once. Defaults to 1 (serial). Raising it is
	 * not sufficient for parallelism: a phase that declares no `dependsOn`
	 * implicitly follows its declaration predecessor, so parallel-eligible
	 * phases must declare dependencies explicitly — `dependsOn: []` marks a
	 * phase as genuinely independent. Writers in a wave run in their own
	 * workspaces and integrate serially. Requires isolation: true and explicit
	 * writes on every agent/fusion phase. To verify combined changes, configure
	 * a downstream code check or verdict_consistent review depending on all
	 * contributing branches; sibling checks verify only their own causal input.
	 * With acceptance: review, the final verdict review must transitively depend
	 * on every contributing writer so its verdict covers their combined changes.
	 */
	"concurrency?": "number",
	/**
	 * `review`: all phases must pass and the last accepted envelope must be
	 * a coherent, approved review. A valid negative review may request a bounded
	 * revision through onReject; otherwise it refuses delivery without retrying
	 * the reviewer. Omitted: passing every phase is sufficient.
	 */
	"acceptance?": '"review"',
	/**
	 * Run every phase against an isolated copy of the repo and apply the result
	 * only if the run is accepted. Without it a rejected workflow leaves its
	 * half-finished edits in the working tree with no undo. Required whenever
	 * concurrency is greater than 1, including code-only workflows.
	 */
	"isolation?": "boolean",
	/**
	 * Globs `diff_matches_claims` treats as always accounted for, for paths a
	 * build legitimately rewrites without any phase claiming them (`bun.lock`,
	 * `generated/**`).
	 *
	 * Empty by default and deliberately so: a wide default makes the gate noisy,
	 * and an operator who cannot tell which changes it forgives stops trusting
	 * it. A malformed pattern fails the run at construction, naming itself.
	 */
	"undeclaredIgnore?": "string[]",
	/**
	 * Globs no phase may change, whatever its `writes` declares. `.omp/adw`
	 * and everything under it is always protected implicitly — a run must not
	 * edit its own workflow state.
	 */
	"protected?": "string[]",
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
