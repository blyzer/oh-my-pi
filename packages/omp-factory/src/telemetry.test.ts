import { describe, expect, it } from "bun:test";
import { selectModel } from "./route";
import { summarize } from "./telemetry";

describe("summarize", () => {
	it("prices a flaky cheap model above a one-shot stronger model", () => {
		const summary = summarize([
			{
				workflow: "w1",
				phase: "build",
				attempt: 1,
				role: "builder",
				model: "cheap",
				provider: "p",
				durationMs: 1,
				tokensIn: 1,
				tokensOut: 1,
				costUsd: 1,
				gateFailures: 1,
				corrections: 1,
				outcome: "rejected",
			},
			{
				workflow: "w1",
				phase: "build",
				attempt: 2,
				role: "builder",
				model: "cheap",
				provider: "p",
				durationMs: 1,
				tokensIn: 1,
				tokensOut: 1,
				costUsd: 1,
				gateFailures: 1,
				corrections: 2,
				outcome: "rejected",
			},
			{
				workflow: "w1",
				phase: "build",
				attempt: 3,
				role: "builder",
				model: "strong",
				provider: "p",
				durationMs: 1,
				tokensIn: 1,
				tokensOut: 1,
				costUsd: 4,
				gateFailures: 0,
				corrections: 2,
				outcome: "accepted",
			},
		]);
		expect(summary.acceptances).toBe(1);
		expect(summary.costPerVerifiedSuccess).toBe(6);
		expect(summary.correctionsPerAcceptance).toBe(2);
		expect(summary.byModel.cheap).toMatchObject({ attempts: 2, acceptances: 0 });
		expect(summary.byModel.strong).toMatchObject({ attempts: 1, acceptances: 1 });
	});

	it("reports null cost when nothing was accepted", () => {
		const summary = summarize([
			{
				workflow: "w1",
				phase: "build",
				attempt: 1,
				role: "builder",
				model: "m",
				provider: "p",
				durationMs: 1,
				tokensIn: 1,
				tokensOut: 1,
				costUsd: 2,
				gateFailures: 1,
				corrections: 0,
				outcome: "rejected",
			},
		]);
		expect(summary.costPerVerifiedSuccess).toBeNull();
		expect(summary.verifiedSuccessRate).toBe(0);
	});
});

describe("selectModel", () => {
	const candidates = [
		{ model: "scout-cheap", capabilities: ["research", "cheap"] as const, costPerAttempt: 1 },
		{ model: "reasoner", capabilities: ["reasoning", "review"] as const, costPerAttempt: 8 },
		{ model: "coder", capabilities: ["coding", "review"] as const, costPerAttempt: 5 },
	];

	it("picks the cheapest candidate covering the required capability", () => {
		expect(selectModel({ role: "reviewer", capability: "review" }, [...candidates])?.model).toBe("coder");
		expect(selectModel({ role: "architect", capability: "reasoning" }, [...candidates])?.model).toBe("reasoner");
	});

	it("returns null when no candidate covers the capability", () => {
		expect(selectModel({ role: "builder", capability: "coding" }, [candidates[0]])).toBeNull();
	});
});
