/**
 * I11: a human gate is a durable pause, not a prompt.
 *
 * The run stops, the process may die, and a later run resolves the decision
 * by nonce. What makes it a gate rather than a suggestion is that the phase
 * does not dispatch while the answer is missing, and that an approval bound
 * to a different attempt cannot be spent here.
 */
import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { loadDecision, requestDecision, resolveDecision } from "./decisions";
import { type GraphPhase, type HumanGateVerdict, runGraph } from "./graph";
import { replay } from "./ledger";

let root = "";
let runDir = "";
let decisionDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-human-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-human-run-"));
	decisionDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-human-dec-"));
}

afterEach(async () => {
	for (const dir of [root, runDir, decisionDir]) {
		if (dir) await fs.rm(dir, { recursive: true, force: true });
	}
	root = "";
	runDir = "";
	decisionDir = "";
});

function releasePhase(ran: string[]): GraphPhase {
	return {
		name: "release",
		scope: ["**"],
		assertions: [],
		maxAttempts: 1,
		requiresHuman: true,
		produce: async () => {
			ran.push("release");
			return { changedFiles: [], label: "release", exitCode: 0 };
		},
	};
}

describe("human gate", () => {
	it("halts the run without dispatching the phase, and records the wait", async () => {
		await makeDirs();
		const ran: string[] = [];
		const decision = await requestDecision(decisionDir, {
			workflow: "wf",
			phase: "release",
			attempt: 1,
			question: "Publish to the registry?",
		});

		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [releasePhase(ran)],
			humanGate: async () => ({
				decision: "pending",
				nonce: decision.nonce,
				question: decision.question,
			}),
		});

		expect(result.status).toBe("awaiting-human");
		// The point of the gate: the producer never ran.
		expect(ran).toEqual([]);
		const release = result.phases.find(outcome => outcome.phase === "release");
		expect(release?.status).toBe("awaiting-human");
		expect(release?.evidence.join(" ")).toContain(decision.nonce);

		// A run waiting on a human is not a failed run, and the durable
		// projection must say so -- an operator reading "failed" would think
		// their work died rather than that it is waiting for them.
		const { projection } = await replay(runDir);
		expect(projection.status).toBe("awaiting-human");
		expect(projection.phases.release?.status).toBe("awaiting-human");
	});

	it("dispatches once the decision is resolved, and refuses a denial", async () => {
		await makeDirs();
		const approved: string[] = [];
		const decision = await requestDecision(decisionDir, {
			workflow: "wf",
			phase: "release",
			attempt: 1,
			question: "Publish?",
		});
		await resolveDecision(decisionDir, decision.nonce, {
			workflow: "wf",
			phase: "release",
			attempt: 1,
			response: "approved",
		});

		// A later process reads the resolution back and lets the phase run.
		const gate = async (phase: string, attempt: number): Promise<HumanGateVerdict> => {
			const stored = await loadDecision(decisionDir, decision.nonce);
			if (!stored?.consumed) return { decision: "pending", nonce: decision.nonce, question: stored?.question ?? "" };
			if (stored.workflow !== "wf" || stored.phase !== phase || stored.attempt !== attempt) {
				return { decision: "denied", reason: "decision belongs to another attempt" };
			}
			return stored.response === "approved"
				? { decision: "approved" }
				: { decision: "denied", reason: "not approved" };
		};

		const accepted = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [releasePhase(approved)],
			humanGate: gate,
		});
		expect(accepted.status).toBe("accepted");
		expect(approved).toEqual(["release"]);

		const denied: string[] = [];
		const deniedRun = await fs.mkdtemp(path.join(os.tmpdir(), "factory-human-run-"));
		const refusal = await runGraph({
			workflowId: "wf",
			runDir: deniedRun,
			workspace: root,
			allowUnguardedWrites: true,
			phases: [releasePhase(denied)],
			humanGate: async () => ({ decision: "denied", reason: "signing key unavailable" }),
		});
		await fs.rm(deniedRun, { recursive: true, force: true });
		expect(refusal.status).toBe("failed");
		expect(denied).toEqual([]);
		expect(refusal.phases[0]?.evidence.join(" ")).toContain("signing key unavailable");
	});
});

describe("decision binding", () => {
	it("refuses an approval bound to a different attempt", async () => {
		await makeDirs();
		const decision = await requestDecision(decisionDir, {
			workflow: "wf",
			phase: "release",
			attempt: 1,
			question: "Publish?",
		});
		// A stale approval is the whole hazard: attempt 1's "yes" must not
		// authorize attempt 2, which asked a different question of a
		// different candidate.
		await expect(
			resolveDecision(decisionDir, decision.nonce, {
				workflow: "wf",
				phase: "release",
				attempt: 2,
				response: "approved",
			}),
		).rejects.toThrow(/stale decision/);
	});

	it("consumes exactly once even when two resolvers race", async () => {
		await makeDirs();
		const decision = await requestDecision(decisionDir, {
			workflow: "wf",
			phase: "release",
			attempt: 1,
			question: "Publish?",
		});
		const resolution = { workflow: "wf", phase: "release", attempt: 1, response: "approved" };
		// load-check-write spans two awaits, and the file backing means the
		// racers can be separate processes: without an exclusive claim both
		// observe `consumed: false` and both succeed.
		const outcomes = await Promise.allSettled([
			resolveDecision(decisionDir, decision.nonce, resolution),
			resolveDecision(decisionDir, decision.nonce, resolution),
		]);
		expect(outcomes.filter(outcome => outcome.status === "fulfilled")).toHaveLength(1);
		expect(outcomes.filter(outcome => outcome.status === "rejected")).toHaveLength(1);
	});

	it("keeps a hostile nonce inside the decision directory", async () => {
		await makeDirs();
		// The path must come from the caller's validated nonce, never from
		// the body: a decision claiming `../../escape` would otherwise direct
		// the write that happens before validation.
		await expect(loadDecision(decisionDir, "../../escape")).rejects.toThrow(/invalid decision nonce/);
	});
});
