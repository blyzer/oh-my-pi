/**
 * Payload schema checks, run by the caller because they need omptype.
 *
 * The JavaScript caller compiles the schema with omptype and reports checks
 * through `noteGateReport`. The Rust engine traces those checks and uses them
 * to decide acceptance alongside its filesystem gates.
 */

import { taskEnvelopeText, type TaskGateCheck, type TaskHandoff } from "@oh-my-pi/pi-natives";
import { fromJsonSchema, type } from "@oh-my-pi/omptype";

/** The gate name the caller reports under, so a reader sees where it came from. */
export const PAYLOAD_GATE = "payload_matches_schema";

export const VERDICT_GATE = "verdict_consistent";

const reviewShape = type({
	approved: "boolean",
	blocking: "string[]",
	findings: type({ requirement: "string", met: "boolean", "evidence?": "string" }).array(),
});

/** Predicates are not represented in JSON Schema; publish the structural shape only. */
export const REVIEW_JSON_SCHEMA = reviewShape.toJsonSchema();

export const REVIEW_RULES = [
	"An approved review must have no blocking entries.",
	"An approved review must have no findings with met=false.",
	"A rejected review must name at least one blocking entry or an unmet requirement.",
	"A coherent rejection is a valid review, but delivery acceptance requires approved=true.",
	"Use status=success when the review completed, even when approved=false; status=fail means the review could not be completed.",
	"These checks establish internal consistency, not the truth or completeness of the review.",
].join("\n");

const reviewSchema = reviewShape.narrow((review, ctx) => {
	let consistent = true;
	if (review.approved) {
		if (review.blocking.length > 0) {
			ctx.error({
				expected: "empty when approved is true",
				relativePath: ["blocking"],
				actual: JSON.stringify(review.blocking),
			});
			consistent = false;
		}
		for (const [index, finding] of review.findings.entries()) {
			if (finding.met) continue;
			ctx.error({
				expected: `true when approved is true (requirement: ${finding.requirement})`,
				relativePath: ["findings", index, "met"],
				actual: finding.met,
			});
			consistent = false;
		}
	} else if (review.blocking.length === 0 && review.findings.every(finding => finding.met)) {
		ctx.error({
			expected: "supported by a blocking entry or a finding with met=false",
			relativePath: ["approved"],
			actual: review.approved,
		});
		consistent = false;
	}
	return consistent;
});

/**
 * Known JSON Schema gaps: conditionals and the remaining listed keywords are
 * ignored; `oneOf` becomes a union, and `not` only supports the special case
 * `not: {}`. Reject them instead of silently weakening their constraints.
 *
 * `anyOf` and `allOf` produce unions and intersections. Their branches still
 * need to fit the importer's dialect; this list is not a conformance check.
 */
const UNSUPPORTED = [
	"if",
	"then",
	"else",
	"not",
	// Accepted as a plain union: a value matching two branches passes.
	"oneOf",
	"dependentSchemas",
	"dependentRequired",
	"patternProperties",
	"propertyNames",
	"unevaluatedProperties",
	"unevaluatedItems",
	"contains",
];

/** Inspect schema locations, not property names, defaults or enum values. */
function unsupportedKeywords(schema: unknown, found: Set<string>): void {
	if (!schema || typeof schema !== "object" || Array.isArray(schema)) return;
	for (const [key, value] of Object.entries(schema)) {
		if (UNSUPPORTED.includes(key)) found.add(key);
		switch (key) {
			case "properties":
			case "patternProperties":
			case "$defs":
			case "definitions":
			case "dependentSchemas":
				if (value && typeof value === "object" && !Array.isArray(value)) {
					for (const child of Object.values(value)) unsupportedKeywords(child, found);
				}
				break;
			case "anyOf":
			case "allOf":
			case "oneOf":
			case "prefixItems":
				if (Array.isArray(value)) {
					for (const child of value) unsupportedKeywords(child, found);
				}
				break;
			case "items":
				if (Array.isArray(value)) {
					for (const child of value) unsupportedKeywords(child, found);
					break;
				}
				unsupportedKeywords(value, found);
				break;
			case "additionalProperties":
			case "additionalItems":
			case "unevaluatedProperties":
			case "unevaluatedItems":
			case "propertyNames":
			case "contains":
			case "not":
			case "if":
			case "then":
			case "else":
				unsupportedKeywords(value, found);
				break;
		}
	}
}

/**
 * Compile a phase's declared schema, or explain why it cannot be used.
 *
 * Called at load time: a malformed schema must fail the workflow file, not the
 * first phase that runs against it — and a schema whose conditionals would be
 * dropped must fail too, rather than reading like a guarantee it is not.
 */
export function compilePayloadSchema(schema: unknown): { validate: (payload: unknown) => string[] } | string {
	const ignored = new Set<string>();
	unsupportedKeywords(schema, ignored);
	if (ignored.size > 0) {
		return `omptype does not enforce ${[...ignored].join(", ")} with full JSON Schema semantics. Use supported constraints in self-contained anyOf/allOf branches, or check the rule in a code phase.`;
	}
	let compiled: ReturnType<typeof fromJsonSchema>;
	try {
		compiled = fromJsonSchema(schema);
	} catch (err) {
		return err instanceof Error ? err.message : String(err);
	}
	return {
		validate: (payload: unknown) => {
			const result = compiled(payload);
			// omptype reports every problem; all of them reach the agent, because
			// fixing one field at a time costs one attempt per field.
			return result instanceof type.errors ? result.map(problem => problem.message) : [];
		},
	};
}

/**
 * Check an agent's turn against the phase schema.
 *
 * The envelope text comes from `taskEnvelopeText` — the engine's own
 * extraction rule — rather than a second implementation of "the last JSON
 * object", which would drift from it silently.
 *
 * An absent or unparseable envelope yields no checks: the engine rejects that
 * on its own with a better message, and reporting it twice would tell the agent
 * it made two mistakes.
 */
export function checkPayload(turn: string, validate: (payload: unknown) => string[]): TaskGateCheck[] {
	const payload = envelopePayload(turn);
	if (payload === undefined) return [];
	return problemChecks(validate(payload), "payload matches the declared schema");
}

/** Caller-owned gate reports and, only for a coherent review, its decision. */
export interface PhaseCheckResult {
	reports: { gate: string; checks: TaskGateCheck[] }[];
	review?: { approved: boolean; reason: string };
}

/** Compile caller-owned checks once, then extract each turn's envelope only once. */
export function compilePhaseChecks(phase: {
	schema?: unknown;
	gates?: readonly string[];
}): { check: (turn: string) => PhaseCheckResult } | string {
	let validatePayload: ((payload: unknown) => string[]) | undefined;
	if (phase.schema !== undefined) {
		const compiled = compilePayloadSchema(phase.schema);
		if (typeof compiled === "string") return compiled;
		validatePayload = compiled.validate;
	}
	const checkReview = phase.gates?.includes(VERDICT_GATE) ?? false;
	return {
		check: turn => {
			const result: PhaseCheckResult = { reports: [] };
			if (!validatePayload && !checkReview) return result;
			const payload = envelopePayload(turn);
			if (payload === undefined) return result;
			if (validatePayload) {
				result.reports.push({
					gate: PAYLOAD_GATE,
					checks: problemChecks(validatePayload(payload), "payload matches the declared schema"),
				});
			}
			if (checkReview) {
				const review = reviewSchema(payload);
				result.reports.push({
					gate: VERDICT_GATE,
					checks: problemChecks(
						review instanceof type.errors ? review.map(problem => problem.message) : [],
						"review verdict is internally consistent",
					),
				});
				if (!(review instanceof type.errors)) result.review = reviewDecision(review);
			}
			return result;
		},
	};
}

/** Evaluate only the last accepted handoff, including one reconstructed on resume. */
export function reviewAcceptance(handoff: TaskHandoff | null): { accepted: boolean; reason: string } {
	if (!handoff) return { accepted: false, reason: "Review acceptance requires a final accepted report." };
	let payload: unknown;
	try {
		payload = JSON.parse(handoff.payloadJson);
	} catch {
		return { accepted: false, reason: "The final review payload is not valid JSON." };
	}
	const review = reviewSchema(payload);
	if (review instanceof type.errors) {
		return { accepted: false, reason: `Invalid final review: ${review.map(problem => problem.message).join("; ")}` };
	}
	const decision = reviewDecision(review);
	return { accepted: decision.approved, reason: decision.reason };
}

/** Describe the review's claims, not independently verified facts. */
function reviewDecision(review: typeof reviewShape.infer): { approved: boolean; reason: string } {
	if (review.approved) return { approved: true, reason: "The review approves delivery." };
	const reasons = [
		...review.blocking.map(blocker => `Reported blocker: ${blocker}`),
		...review.findings
			.filter(finding => !finding.met)
			.map(
				finding =>
					`Reported unmet requirement: ${finding.requirement}${finding.evidence ? ` (review evidence: ${finding.evidence})` : ""}`,
			),
	];
	return { approved: false, reason: `The review rejects delivery: ${reasons.join("; ")}` };
}

function envelopePayload(turn: string): unknown {
	const text = taskEnvelopeText(turn);
	if (!text) return undefined;
	try {
		return JSON.parse(text);
	} catch {
		return undefined;
	}
}

function problemChecks(problems: string[], success: string): TaskGateCheck[] {
	if (problems.length === 0) {
		// Names what it examined, so a green check is evidence and not silence.
		return [{ item: ".", ok: true, note: success }];
	}
	return problems.map(problem => ({ item: fieldOf(problem), ok: false, note: problem }));
}

/**
 * The field a problem is about, for the check's `item`.
 *
 * A violation that says only "expected boolean" makes the agent guess which
 * field; omptype puts the path first, so the leading token is the answer.
 */
function fieldOf(problem: string): string {
	const first = problem.trim().split(/\s+/)[0] ?? ".";
	return first.replace(/[:,]$/, "") || ".";
}
