/**
 * Behavioral equivalence with the frozen ADW oracle (tag
 * `adw-prototype-reference`, `cargo test -p pi-tasks`, 74 tests). Each case
 * here names the oracle behavior it reproduces through the new driver.
 * Internal traces differ by design; the acceptance semantics must not.
 */
import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { buildWaves, readyPhases, VersionStore } from "./dag";
import { evaluateFileAssertions } from "./gates";
import { replay } from "./ledger";
import { runWorkflow } from "./workflow";

let root = "";
let runDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-equiv-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-equiv-run-"));
}

afterEach(async () => {
	if (root) await fs.rm(root, { recursive: true, force: true });
	if (runDir) await fs.rm(runDir, { recursive: true, force: true });
	root = "";
	runDir = "";
});

describe("FB-GATE — a failed gate re-runs the same phase with the violation named", () => {
	it("feeds the exact gate failure into the next attempt", async () => {
		await makeDirs();
		const corrections: Array<string | undefined> = [];
		let attempt = 0;
		const result = await runWorkflow({
			workflowId: "wf",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [{ type: "file_contains", file: "out.txt", marker: "DONE" }],
			maxAttempts: 3,
			produce: async (_n, evidence) => {
				corrections.push(evidence);
				attempt += 1;
				if (attempt === 2) await Bun.write(path.join(root, "out.txt"), "DONE");
				return { changedFiles: [], label: `c${attempt}`, exitCode: 0 };
			},
		});
		expect(result.status).toBe("accepted");
		expect(corrections[1]).toContain("missing artifact");
		expect(corrections[1]).toContain("out.txt");
	});
});

describe("FB-GATE — claiming nothing does not clear the gate", () => {
	it("rejects an empty artifact declaration on a phase that must produce files", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			requireArtifacts: true,
			maxAttempts: 1,
			produce: async () => ({ changedFiles: [], declaredArtifacts: [], label: "c1", exitCode: 0 }),
		});
		expect(result.status).toBe("rejected");
		expect(result.evidence[0]).toContain("declared none");
	});
});

describe("FB-GATE — bytes are not structure / empty file passes existence but fails non-empty", () => {
	it("separates existence, emptiness and JSON structure", async () => {
		await makeDirs();
		await Bun.write(path.join(root, "plan.json"), "");
		const empty = await evaluateFileAssertions([{ type: "file_contains", file: "plan.json", marker: "{" }], root);
		expect(empty.passed).toBeFalse();
		await Bun.write(path.join(root, "plan.json"), '{"step": 1,}');
		const malformed = await evaluateFileAssertions([{ type: "json_parses", file: "plan.json" }], root);
		expect(malformed.passed).toBeFalse();
		expect(malformed.failures[0]).toContain("unknown assertion type");
	});
});

describe("FB-FAILCLOSED — a self-reported failure is rejected even with green gates", () => {
	it("never accepts a builder that declared failure", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 1,
			produce: async () => ({
				changedFiles: [],
				label: "c1",
				exitCode: 0,
				selfReportedStatus: "fail" as const,
				summary: "gave up",
			}),
			review: async () => ({ approved: true, blockers: [], findings: [] }),
		});
		expect(result.status).toBe("rejected");
	});
});

describe("FB-CORRECTION — unparseable output is a correction, not a crash", () => {
	it("rejects the attempt, retries, and accepts a conforming envelope", async () => {
		await makeDirs();
		let attempt = 0;
		const result = await runWorkflow({
			workflowId: "wf",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 2,
			produce: async () => {
				attempt += 1;
				return attempt === 1
					? { changedFiles: [], label: "c1", exitCode: 0, envelopeViolation: "no top-level JSON object" }
					: { changedFiles: [], label: "c2", exitCode: 0, selfReportedStatus: "success" as const };
			},
		});
		expect(result.status).toBe("accepted");
		expect(result.attempts).toBe(2);
		expect(result.evidence[0]).toContain("envelope violation");
	});
});

describe("FB-CORRECTION — exhausting attempts halts the run", () => {
	it("stops at the declared budget and records the trail", async () => {
		await makeDirs();
		let produced = 0;
		const result = await runWorkflow({
			workflowId: "wf",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["src/**"],
			assertions: [],
			maxAttempts: 3,
			produce: async () => {
				produced += 1;
				return { changedFiles: ["README.md"], label: `c${produced}`, exitCode: 0 };
			},
		});
		expect(result.status).toBe("rejected");
		expect(produced).toBe(3);
		const { projection } = await replay(runDir);
		expect(projection.phases.build?.attempts).toBe(3);
	});
});

describe("FB-DAG — dependencies reorder declaration order", () => {
	it("schedules by the declared graph, not the file order", () => {
		const deps = { docs: ["api"], verify: ["docs"], api: [] };
		expect(buildWaves(Object.keys(deps), deps)).toEqual([["api"], ["docs"], ["verify"]]);
	});
});

describe("FB-DAG — an unorderable graph is refused, not invented", () => {
	it("names every phase in the cycle", () => {
		expect(() => buildWaves(["a", "b"], { a: ["b"], b: ["a"] })).toThrow("dependency cycle among phases: a, b");
	});
});

describe("FB-HANDOFF — a revision supersedes the old version and reselects the new one", () => {
	it("keeps the consumed version stable and advances the current one", () => {
		const store = new VersionStore();
		store.accept({ phase: "plan", version: 1, digest: "d1", artifacts: ["plan.md"] });
		const consumed = store.select("plan", 1);
		store.accept({ phase: "plan", version: 2, digest: "d2", artifacts: ["plan.md"] });
		expect(consumed?.digest).toBe("d1");
		expect(store.select("plan")?.digest).toBe("d2");
	});
});

describe("FB-HANDOFF — an invalidated producer without a successor is refused, not substituted", () => {
	it("returns null instead of an older version", () => {
		const store = new VersionStore();
		expect(store.select("plan", 2)).toBeNull();
	});
});

describe("FB-DAG — a failed dependency keeps its descendants blocked", () => {
	it("does not ready a consumer whose producer failed permanently", () => {
		const deps = { plan: [], build: ["plan"], review: ["build"] };
		expect(readyPhases(deps, new Set(["plan"]), new Set(["plan", "build"]))).toEqual([]);
	});
});

describe("FB-RECOVERY — an in-flight attempt replays as dispatched, never as accepted", () => {
	it("reconstructs a killed run without inventing acceptance", async () => {
		await makeDirs();
		await runWorkflow({
			workflowId: "wf",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 1,
			produce: async () => {
				throw new Error("killed mid-flight");
			},
		});
		const { projection } = await replay(runDir);
		expect(projection.status).toBe("failed");
		expect(projection.phases.build?.acceptedVersion).toBeNull();
	});
});
