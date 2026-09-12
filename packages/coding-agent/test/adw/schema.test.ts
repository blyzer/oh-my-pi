import { describe, expect, it } from "bun:test";
import {
	checkPayload,
	compilePayloadSchema,
	compilePhaseChecks,
	PAYLOAD_GATE,
	REVIEW_JSON_SCHEMA,
	reviewAcceptance,
	VERDICT_GATE,
} from "@oh-my-pi/pi-coding-agent/adw/schema";
import type { TaskHandoff } from "@oh-my-pi/pi-natives";

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
		const checks = checkPayload(turn('"approved":"yes","blocking":"nope"'), validator());
		expect(
			checks
				.filter(check => !check.ok)
				.map(check => check.item)
				.sort(),
		).toEqual(["approved", "blocking"]);
	});

	it("catches a missing required field", () => {
		const checks = checkPayload(turn('"approved":true'), validator());
		expect(checks.some(check => !check.ok && check.item.includes("blocking"))).toBe(true);
	});

	it("passes a structurally valid payload without imposing a cross-field rule", () => {
		const checks = checkPayload(turn('"approved":true,"blocking":["x"]'), validator());
		expect(checks).toHaveLength(1);
		expect(checks[0]?.ok).toBe(true);
	});

	it("stays silent when there is no envelope to check", () => {
		// The engine rejects an unparseable turn with a better message. Reporting
		// it here too would tell the agent it made two mistakes.
		expect(checkPayload("I could not do it.", validator())).toHaveLength(0);
		expect(checkPayload("prose {not json} more", validator())).toHaveLength(0);
	});

	it("validates the final envelope rather than an earlier example", () => {
		const twice = `Here is an example {"approved":"wrong"} and now the real one:\n{"status":"success","summary":"s","artifacts":[],"approved":true,"blocking":[]}`;
		const checks = checkPayload(twice, validator());
		expect(checks).toHaveLength(1);
		expect(checks[0]?.ok).toBe(true);
	});
});

describe("compilePayloadSchema", () => {
	it("refuses a conditional omptype would drop, naming the keywords", () => {
		// oxlint-disable-next-line unicorn/no-thenable -- JSON Schema if/then/else keyword
		const result = compilePayloadSchema({ type: "object", if: { const: 1 }, then: { const: 2 } });
		expect(typeof result).toBe("string");
		expect(result as string).toContain("if");
		expect(result as string).toContain("then");
	});

	it("requires a value to match at least one anyOf branch", () => {
		const compiled = compilePayloadSchema({ anyOf: [{ type: "string" }, { type: "number" }] });
		if (typeof compiled === "string") throw new Error(compiled);
		expect(compiled.validate(true).length).toBeGreaterThan(0);
		expect(compiled.validate("ok")).toHaveLength(0);
	});

	it("enforces both bounds in explicitly typed allOf branches", () => {
		const compiled = compilePayloadSchema({
			allOf: [
				{ type: "number", minimum: 5 },
				{ type: "number", maximum: 10 },
			],
		});
		if (typeof compiled === "string") throw new Error(compiled);
		expect(compiled.validate(1).length).toBeGreaterThan(0);
		expect(compiled.validate(7)).toHaveLength(0);
		expect(compiled.validate(12).length).toBeGreaterThan(0);
	});

	it("rejects approval with blockers using self-contained anyOf branches", () => {
		const compiled = compilePayloadSchema({
			anyOf: [
				{
					...SCHEMA,
					properties: { ...SCHEMA.properties, approved: { const: false } },
				},
				{
					...SCHEMA,
					properties: {
						approved: { const: true },
						blocking: { type: "array", items: { type: "string" }, maxItems: 0 },
					},
				},
			],
		});
		if (typeof compiled === "string") throw new Error(compiled);
		expect(checkPayload(turn('"approved":true,"blocking":["x"]'), compiled.validate).some(check => !check.ok)).toBe(
			true,
		);
		const accepted = checkPayload(turn('"approved":true,"blocking":[]'), compiled.validate);
		expect(accepted).toHaveLength(1);
		expect(accepted[0]?.ok).toBe(true);
		expect(compiled.validate({ approved: false, blocking: ["x"] })).toHaveLength(0);
		expect(compiled.validate({ approved: false }).length).toBeGreaterThan(0);
		expect(compiled.validate({ approved: "yes", blocking: [] }).length).toBeGreaterThan(0);
	});

	it("distinguishes schema keywords from property names and annotation data", () => {
		const compiled = compilePayloadSchema({
			type: "object",
			required: ["if"],
			properties: {
				if: { type: "boolean" },
				metadata: { type: "object", examples: [{ oneOf: ["data"] }] },
			},
		});
		if (typeof compiled === "string") throw new Error(compiled);
		expect(compiled.validate({ if: true })).toHaveLength(0);
		expect(compiled.validate({ if: "yes" }).length).toBeGreaterThan(0);
	});

	it("rejects unsupported constraints inside referenced definitions", () => {
		const compiled = compilePayloadSchema({
			type: "array",
			items: { $ref: "#/$defs/item" },
			$defs: { item: { oneOf: [{ type: "string" }, { type: "number" }] } },
		});
		expect(typeof compiled).toBe("string");
		expect(compiled as string).toContain("oneOf");
	});

	it("enforces a typed numeric minimum", () => {
		const compiled = compilePayloadSchema({ type: "object", properties: { n: { type: "number", minimum: 5 } } });
		if (typeof compiled === "string") throw new Error(compiled);
		expect(compiled.validate({ n: 1 }).length).toBeGreaterThan(0);
		expect(compiled.validate({ n: 9 })).toHaveLength(0);
	});
});

describe("review verdict checks", () => {
	function checks(payload: Record<string, unknown>) {
		const compiled = compilePhaseChecks({ gates: [VERDICT_GATE] });
		if (typeof compiled === "string") throw new Error(compiled);
		return compiled.check(JSON.stringify({ status: "success", summary: "review", ...payload })).reports[0]!.checks;
	}

	it("collects blockers and every unmet requirement contradicting approval", () => {
		const result = checks({
			approved: true,
			blocking: ["Authentication can be bypassed"],
			findings: [
				{ requirement: "Require authentication", met: false },
				{ requirement: "Record denied access", met: false },
			],
		});
		expect(result.filter(check => !check.ok).map(check => check.item)).toEqual([
			"blocking",
			"findings[0].met",
			"findings[1].met",
		]);
		expect(result.find(check => check.item === "findings[0].met")?.note).toContain("Require authentication");
	});

	it("rejects a negative verdict unsupported by blockers or unmet requirements", () => {
		const result = checks({
			approved: false,
			blocking: [],
			findings: [{ requirement: "Require authentication", met: true }],
		});
		expect(result.some(check => !check.ok && check.item === "approved")).toBe(true);
	});

	it("accepts a coherent rejection supported by either a blocker or an unmet requirement", () => {
		for (const payload of [
			{ approved: false, blocking: ["Missing authentication"], findings: [] },
			{
				approved: false,
				blocking: [],
				findings: [{ requirement: "Require authentication", met: false }],
			},
		]) {
			expect(checks(payload).map(check => check.ok)).toEqual([true]);
		}
	});

	it("preserves all missing-field failures without filling review defaults", () => {
		expect(
			checks({})
				.map(check => check.item)
				.sort(),
		).toEqual(["approved", "blocking", "findings"]);
		expect(checks({}).every(check => !check.ok)).toBe(true);
	});

	it("preserves typed failures including nested requirement, verdict and evidence paths", () => {
		const result = checks({
			approved: "yes",
			blocking: [7],
			findings: [{ requirement: 1, met: "no", evidence: false }],
		});
		expect(
			result
				.filter(check => !check.ok)
				.map(check => check.item)
				.sort(),
		).toEqual(["approved", "blocking[0]", "findings[0].evidence", "findings[0].met", "findings[0].requirement"]);
		expect(
			checks({ approved: true, blocking: [], findings: [{}] })
				.filter(check => !check.ok)
				.map(check => check.item)
				.sort(),
		).toEqual(["findings[0].met", "findings[0].requirement"]);
		expect(checks({ approved: true, blocking: [], findings: null }).some(check => !check.ok)).toBe(true);
	});

	it("exports structural review constraints without pretending JSON Schema enforces predicates", () => {
		const compiled = compilePayloadSchema(REVIEW_JSON_SCHEMA);
		if (typeof compiled === "string") throw new Error(compiled);
		expect(compiled.validate({ approved: true, blocking: ["Contradiction"], findings: [] })).toEqual([]);
		expect(compiled.validate({ approved: true, blocking: [] }).some(problem => problem.includes("findings"))).toBe(
			true,
		);
	});
});

describe("compilePhaseChecks", () => {
	it("reports independent schema and verdict failures against the same final envelope", () => {
		const compiled = compilePhaseChecks({
			schema: { type: "object", required: ["score"], properties: { score: { type: "number" } } },
			gates: [VERDICT_GATE],
		});
		if (typeof compiled === "string") throw new Error(compiled);
		const { reports, review } = compiled.check(
			`${turn('"score":1,"approved":true,"blocking":[],"findings":[]')}\n${turn('"score":"high","approved":true,"blocking":["Unsafe"],"findings":[]')}`,
		);
		expect(reports.map(report => report.gate)).toEqual([PAYLOAD_GATE, VERDICT_GATE]);
		expect(reports[0]?.checks.some(check => !check.ok && check.item === "score")).toBe(true);
		expect(reports[1]?.checks.some(check => !check.ok && check.item === "blocking")).toBe(true);
		expect(review).toBeUndefined();
		const passing = compiled.check(turn('"score":1,"approved":true,"blocking":[],"findings":[]'));
		expect(passing.reports.map(report => report.checks.map(check => check.ok))).toEqual([[true], [true]]);
		expect(passing.review?.approved).toBe(true);
		expect(compiled.check("No envelope was produced")).toEqual({ reports: [] });
		expect(compiled.check("{not json}")).toEqual({ reports: [] });
	});

	it("returns a coherent rejection with reported evidence without hiding independent schema failures", () => {
		const compiled = compilePhaseChecks({
			schema: { type: "object", required: ["score"], properties: { score: { type: "number" } } },
			gates: [VERDICT_GATE],
		});
		if (typeof compiled === "string") throw new Error(compiled);
		const result = compiled.check(
			turn(
				'"score":"high","approved":false,"blocking":["Authentication bypass"],"findings":[{"requirement":"Record denied access","met":false,"evidence":"No audit entry after denied request"}]',
			),
		);
		expect(result.reports[0]?.checks.some(check => !check.ok && check.item === "score")).toBe(true);
		expect(result.reports[1]?.checks.every(check => check.ok)).toBe(true);
		expect(result.review?.approved).toBe(false);
		expect(result.review?.reason).toContain("Authentication bypass");
		expect(result.review?.reason).toContain("Record denied access");
		expect(result.review?.reason).toContain("No audit entry after denied request");
	});

	it("never supplies a routing decision for a structurally invalid or unsupported verdict", () => {
		const compiled = compilePhaseChecks({ gates: [VERDICT_GATE] });
		if (typeof compiled === "string") throw new Error(compiled);
		for (const payload of ['"approved":false,"blocking":[]', '"approved":false,"blocking":[],"findings":[]']) {
			const result = compiled.check(turn(payload));
			expect(result.reports[0]?.checks.some(check => !check.ok)).toBe(true);
			expect(result.review).toBeUndefined();
		}
	});

	it("does not infer a review decision from review-shaped data without the verdict gate", () => {
		const compiled = compilePhaseChecks({ schema: SCHEMA });
		if (typeof compiled === "string") throw new Error(compiled);
		const result = compiled.check(turn('"approved":false,"blocking":["Unsafe"],"findings":[]'));
		expect(result.reports.map(report => report.gate)).toEqual([PAYLOAD_GATE]);
		expect(result.review).toBeUndefined();
	});
});

describe("reviewAcceptance", () => {
	function handoff(payload: unknown): TaskHandoff {
		return { summary: "Review", artifacts: [], notesForNextAgent: "", payloadJson: JSON.stringify(payload) };
	}

	it("refuses a finding that claims verification without naming what was checked", () => {
		// `basis` exists to keep a model's reading from being read later as a
		// test result. Unenforced it becomes a label a reviewer applies to make
		// its opinion carry more weight, which inverts the point.
		expect(
			reviewAcceptance(
				handoff({
					approved: true,
					blocking: [],
					findings: [{ requirement: "tests pass", met: true, basis: "verified" }],
				}),
			).accepted,
		).toBe(false);
	});

	it("accepts a verified finding that cites its evidence", () => {
		expect(
			reviewAcceptance(
				handoff({
					approved: true,
					blocking: [],
					findings: [{ requirement: "tests pass", met: true, basis: "verified", evidence: "bun test: 252 pass" }],
				}),
			).accepted,
		).toBe(true);
	});

	it("treats a judged finding, and an unmarked one, as opinion needing no evidence", () => {
		// Absent basis means judged: a finding that does not claim to be
		// verified is not, and asking opinion to cite a command it never ran
		// would only teach reviewers to invent one.
		for (const findings of [
			[{ requirement: "readable", met: true, basis: "judged" }],
			[{ requirement: "readable", met: true }],
		]) {
			expect(reviewAcceptance(handoff({ approved: true, blocking: [], findings })).accepted).toBe(true);
		}
	});

	it("accepts a coherent positive final report after handoff serialization", () => {
		const restored: TaskHandoff = JSON.parse(
			JSON.stringify(
				handoff({
					approved: true,
					blocking: [],
					findings: [
						{ requirement: "Require authentication", met: true, evidence: "Unauthenticated request denied" },
					],
				}),
			),
		);
		expect(reviewAcceptance(restored).accepted).toBe(true);
	});

	it("denies delivery for a coherent negative report and preserves its reasons", () => {
		const result = reviewAcceptance(
			handoff({
				approved: false,
				blocking: ["Authentication bypass"],
				findings: [{ requirement: "Record denied access", met: false }],
			}),
		);
		expect(result.accepted).toBe(false);
		expect(result.reason).toContain("Authentication bypass");
		expect(result.reason).toContain("Record denied access");
	});

	it("fails closed for absent, malformed, structurally invalid or inconsistent final reports", () => {
		for (const report of [
			null,
			{ ...handoff({}), payloadJson: "not JSON" },
			handoff(null),
			handoff({ approved: true, blocking: [] }),
			handoff({ approved: "true", blocking: [], findings: [] }),
			handoff({ approved: true, blocking: ["Unsafe"], findings: [] }),
			handoff({ approved: true, blocking: [], findings: [{ requirement: "Authentication", met: false }] }),
			handoff({ approved: false, blocking: [], findings: [] }),
		]) {
			expect(reviewAcceptance(report).accepted).toBe(false);
		}
	});
});
