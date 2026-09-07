/**
 * Payload schema checks, run by the caller because they need omptype.
 *
 * The engine judges every gate but can only *run* the ones needing nothing
 * beyond the filesystem. A JSON Schema validator inside `pi-tasks` would cost
 * that crate its three dependencies and its ability to be tested without a
 * JavaScript runtime, so this check follows the bargain `code` phases already
 * make: the caller executes, reports back through `noteGateReport`, and the
 * engine treats the verdict exactly like a native gate — traced as a
 * `gate_check`, blocking acceptance.
 */

import { taskEnvelopeText, type TaskGateCheck } from "@oh-my-pi/pi-natives";
import { fromJsonSchema, type } from "@oh-my-pi/omptype";

/** The gate name the caller reports under, so a reader sees where it came from. */
export const PAYLOAD_GATE = "payload_matches_schema";

/**
 * JSON Schema keywords omptype accepts syntactically and then ignores.
 *
 * Measured, not assumed: `fromJsonSchema` compiles a schema carrying
 * `if`/`then` without complaint and validates `{approved: true, blocking:
 * ["x"]}` as valid. A conditional that silently does nothing is worse than an
 * absent one — it reads like a guarantee. `type`, `required`, `properties`,
 * `items`, `enum`, `const` and the numeric/length bounds do work.
 */
const UNSUPPORTED = [
	"if",
	"then",
	"else",
	"allOf",
	"anyOf",
	"oneOf",
	"not",
	"dependentSchemas",
	"dependentRequired",
	"patternProperties",
	"propertyNames",
	"unevaluatedProperties",
	"unevaluatedItems",
	"contains",
];

/** Every unsupported keyword anywhere in the document, in first-seen order. */
function unsupportedKeywords(schema: unknown, found = new Set<string>()): string[] {
	if (Array.isArray(schema)) {
		for (const item of schema) unsupportedKeywords(item, found);
	} else if (schema && typeof schema === "object") {
		for (const [key, value] of Object.entries(schema)) {
			if (UNSUPPORTED.includes(key)) found.add(key);
			unsupportedKeywords(value, found);
		}
	}
	return [...found];
}

/**
 * Compile a phase's declared schema, or explain why it cannot be used.
 *
 * Called at load time: a malformed schema must fail the workflow file, not the
 * first phase that runs against it — and a schema whose conditionals would be
 * dropped must fail too, rather than reading like a guarantee it is not.
 */
export function compilePayloadSchema(schema: unknown): { validate: (payload: unknown) => string[] } | string {
	const ignored = unsupportedKeywords(schema);
	if (ignored.length > 0) {
		return `omptype does not enforce ${ignored.join(", ")} — it would compile and then validate anything. Express the constraint with type/required/properties/items/enum/const, or check it in a code phase.`;
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
	const text = taskEnvelopeText(turn);
	if (!text) return [];
	let payload: unknown;
	try {
		payload = JSON.parse(text);
	} catch {
		return [];
	}
	const problems = validate(payload);
	if (problems.length === 0) {
		// Names what it examined, so a green check is evidence and not silence.
		return [{ item: ".", ok: true, note: "payload matches the declared schema" }];
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
