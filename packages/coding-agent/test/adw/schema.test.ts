import { describe, expect, it } from "bun:test";
import { checkPayload, compilePayloadSchema, PAYLOAD_GATE } from "@oh-my-pi/pi-coding-agent/adw/schema";

const SCHEMA = {
	type: "object",
	required: ["approved", "blocking"],
	properties: {
		approved: { type: "boolean" },
		blocking: { type: "array", items: { type: "string" } },
	},
};

function validator() {
	const compiled = compilePayloadSchema(SCHEMA);
	if (typeof compiled === "string") throw new Error(compiled);
	return compiled.validate;
}

/** A turn as an agent really produces it: prose, then the envelope. */
function turn(payload: string): string {
	return `I reviewed the file and wrote it up.\n\n{"status":"success","summary":"s","artifacts":["REVIEW.md"],${payload}}`;
}

describe("checkPayload", () => {
	it("names the field that is wrong, not just that something is", () => {
		// A violation saying only "expected boolean" makes the agent guess which
		// field, and guessing costs an attempt.
		const checks = checkPayload(turn('"approved":"yes","blocking":[]'), validator());
		expect(checks.length).toBeGreaterThan(0);
		expect(checks.every(check => !check.ok)).toBe(true);
		expect(checks.some(check => check.item.includes("approved"))).toBe(true);
	});

	it("reports every problem at once", () => {
		// One field per attempt would burn the budget on a schema with four.
		const checks = checkPayload(turn('"approved":"yes","blocking":"nope"'), validator());
		expect(checks.filter(check => !check.ok).length).toBeGreaterThan(1);
	});

	it("catches a missing required field", () => {
		const checks = checkPayload(turn('"approved":true'), validator());
		expect(checks.some(check => !check.ok && check.item.includes("blocking"))).toBe(true);
	});

	it("passes a correct payload with a check that says what it examined", () => {
		const checks = checkPayload(turn('"approved":true,"blocking":["x"]'), validator());
		expect(checks).toHaveLength(1);
		expect(checks[0]?.ok).toBe(true);
		// Evidence rather than silence, same contract as the native gates.
		expect(checks[0]?.note).toContain("schema");
	});

	it("stays silent when there is no envelope to check", () => {
		// The engine rejects an unparseable turn with a better message. Reporting
		// it here too would tell the agent it made two mistakes.
		expect(checkPayload("I could not do it.", validator())).toHaveLength(0);
		expect(checkPayload("prose {not json} more", validator())).toHaveLength(0);
	});

	it("reads the envelope by the engine's own rule, not a second one", () => {
		// Two objects in the turn: the envelope is the last top-level one, which
		// is the rule `taskEnvelopeText` implements. A local reimplementation
		// would likely take the first and diverge silently.
		const twice = `Here is an example {"approved":"wrong"} and now the real one:\n{"status":"success","summary":"s","artifacts":[],"approved":true,"blocking":[]}`;
		const checks = checkPayload(twice, validator());
		expect(checks).toHaveLength(1);
		expect(checks[0]?.ok).toBe(true);
	});
});

describe("compilePayloadSchema", () => {
	it("refuses a conditional omptype would drop, naming the keywords", () => {
		const result = compilePayloadSchema({ type: "object", if: { const: 1 }, then: { const: 2 } });
		expect(typeof result).toBe("string");
		expect(result as string).toContain("if");
		expect(result as string).toContain("then");
	});

	it("names the gate it reports under", () => {
		expect(PAYLOAD_GATE).toBe("payload_matches_schema");
	});
});
