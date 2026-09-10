import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { loadDecision, requestDecision, resolveDecision } from "./decisions";
import { combineGateAndReview } from "./review";

let dir = "";

async function makeDir(): Promise<string> {
	dir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-decisions-"));
	return dir;
}

afterEach(async () => {
	if (dir) await fs.rm(dir, { recursive: true, force: true });
	dir = "";
});

describe("combineGateAndReview", () => {
	it("reviewer approval never overrides a gate failure", () => {
		const decision = combineGateAndReview(false, ["marker missing"], {
			approved: true,
			blockers: [],
			findings: [],
		});
		expect(decision.accepted).toBeFalse();
		if (!decision.accepted) {
			expect(decision.needsRevision).toBeTrue();
			expect(decision.reason).toContain("marker missing");
		}
	});

	it("reviewer blockers send a gate-passing candidate back for revision", () => {
		const decision = combineGateAndReview(true, [], {
			approved: false,
			blockers: ["API shape unclear"],
			findings: [],
		});
		expect(decision.accepted).toBeFalse();
		if (!decision.accepted) expect(decision.reason).toContain("API shape unclear");
	});

	it("accepts only gate-pass plus clean approval", () => {
		expect(combineGateAndReview(true, [], { approved: true, blockers: [], findings: ["nit"] })).toEqual({
			accepted: true,
		});
	});
});

describe("human decisions", () => {
	it("resolves exactly once with matching binding", async () => {
		const root = await makeDir();
		const pending = await requestDecision(root, {
			workflow: "wf",
			phase: "design",
			attempt: 2,
			question: "Which API shape?",
		});
		expect(pending.response).toBeNull();
		const resolved = await resolveDecision(root, pending.nonce, {
			workflow: "wf",
			phase: "design",
			attempt: 2,
			response: "shape B",
		});
		expect(resolved.response).toBe("shape B");
		expect((await loadDecision(root, pending.nonce))?.consumed).toBeTrue();
		await expect(
			resolveDecision(root, pending.nonce, { workflow: "wf", phase: "design", attempt: 2, response: "again" }),
		).rejects.toThrow("already consumed");
	});

	it("rejects a stale approval bound to an earlier attempt", async () => {
		const root = await makeDir();
		const pending = await requestDecision(root, {
			workflow: "wf",
			phase: "design",
			attempt: 1,
			question: "Which API shape?",
		});
		await expect(
			resolveDecision(root, pending.nonce, { workflow: "wf", phase: "design", attempt: 2, response: "shape B" }),
		).rejects.toThrow("stale decision");
	});

	it("rejects unknown nonces", async () => {
		const root = await makeDir();
		await expect(
			resolveDecision(root, "nonexistent-nonce", { workflow: "wf", phase: "p", attempt: 1, response: "x" }),
		).rejects.toThrow("unknown decision nonce");
	});
});
