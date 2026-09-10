import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { type GraphPhase, runGraph } from "./graph";
import { replay } from "./ledger";

let root = "";
let runDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-graph-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-graph-run-"));
}

afterEach(async () => {
	if (root) await fs.rm(root, { recursive: true, force: true });
	if (runDir) await fs.rm(runDir, { recursive: true, force: true });
	root = "";
	runDir = "";
});

function phase(name: string, overrides: Partial<GraphPhase> = {}): GraphPhase {
	return {
		name,
		scope: ["**"],
		assertions: [],
		maxAttempts: 1,
		produce: async () => ({ changedFiles: [], label: name, exitCode: 0 }),
		...overrides,
	};
}

describe("runGraph", () => {
	it("runs a diamond in dependency order and accepts every phase", async () => {
		await makeDirs();
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [
				phase("reviewer", { dependsOn: ["backend", "frontend"] }),
				phase("backend", { dependsOn: ["architect"] }),
				phase("frontend", { dependsOn: ["architect"] }),
				phase("architect"),
			],
		});
		expect(result.status).toBe("accepted");
		expect(result.order).toEqual(["architect", "backend", "frontend", "reviewer"]);
	});

	it("blocks a descendant when its dependency never reaches an accepted version", async () => {
		await makeDirs();
		const ran: string[] = [];
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [
				phase("architect", {
					produce: async () => {
						ran.push("architect");
						return { changedFiles: [], label: "architect", exitCode: 0 };
					},
				}),
				phase("backend", {
					dependsOn: ["architect"],
					produce: async () => {
						ran.push("backend");
						return { changedFiles: [], label: "backend", exitCode: 1, output: "boom" };
					},
				}),
				phase("frontend", {
					dependsOn: ["architect"],
					produce: async () => {
						ran.push("frontend");
						return { changedFiles: [], label: "frontend", exitCode: 0 };
					},
				}),
				phase("reviewer", { dependsOn: ["backend", "frontend"] }),
			],
		});
		expect(result.status).toBe("failed");
		// architect gates the wave; backend and frontend are concurrent readers,
		// so membership is the contract and their finish order is not.
		expect(ran[0]).toBe("architect");
		expect([...ran].sort()).toEqual(["architect", "backend", "frontend"]);
		const reviewer = result.phases.find(outcome => outcome.phase === "reviewer");
		expect(reviewer?.status).toBe("blocked");
		expect(reviewer?.evidence[0]).toContain("backend");
		expect(result.phases.find(outcome => outcome.phase === "frontend")?.status).toBe("accepted");
	});

	it("hands a consumer the exact accepted version of each declared input", async () => {
		await makeDirs();
		const seen: Array<{ phase: string; version: number }> = [];
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [
				phase("plan", {
					produce: async () => ({
						changedFiles: ["plan.md"],
						declaredArtifacts: ["plan.md"],
						label: "plan",
						exitCode: 0,
						summary: "planned",
					}),
				}),
				phase("build", {
					dependsOn: ["plan"],
					inputs: ["plan"],
					produce: async ({ inputs }) => {
						for (const input of inputs) seen.push({ phase: input.phase, version: input.version });
						return { changedFiles: [], label: "build", exitCode: 0 };
					},
				}),
			],
		});
		expect(result.status).toBe("accepted");
		expect(seen).toEqual([{ phase: "plan", version: 1 }]);
		const build = result.phases.find(outcome => outcome.phase === "build");
		expect(build?.inputs[0]?.artifacts).toEqual(["plan.md"]);
	});

	it("records every phase attempt in one shared ledger", async () => {
		await makeDirs();
		await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [phase("plan"), phase("build", { dependsOn: ["plan"] })],
		});
		const { projection } = await replay(runDir);
		expect(projection.phases.plan).toMatchObject({ status: "accepted", acceptedVersion: 1 });
		expect(projection.phases.build).toMatchObject({ status: "accepted", acceptedVersion: 1 });
	});

	it("refuses a cycle before running anything", async () => {
		await makeDirs();
		let produced = 0;
		const counting = async () => {
			produced += 1;
			return { changedFiles: [], label: "x", exitCode: 0 };
		};
		await expect(
			runGraph({
				workflowId: "wf",
				runDir,
				workspace: root,
				phases: [
					phase("a", { dependsOn: ["b"], produce: counting }),
					phase("b", { dependsOn: ["a"], produce: counting }),
				],
			}),
		).rejects.toThrow("dependency cycle among phases: a, b");
		expect(produced).toBe(0);
	});

	it("refuses an input that is not a declared dependency", async () => {
		await makeDirs();
		await expect(
			runGraph({
				workflowId: "wf",
				runDir,
				workspace: root,
				phases: [phase("plan"), phase("build", { inputs: ["plan"] })],
			}),
		).rejects.toThrow('consumes "plan" without depending on it');
	});

	it("overlaps readers in a wave but never two writers", async () => {
		await makeDirs();
		let inFlight = 0;
		let peakReaders = 0;
		let peakWriters = 0;
		// A barrier, not a sleep: readers only proceed once both have entered,
		// so overlap is observed rather than raced for under load.
		let release: () => void = () => {};
		const bothEntered = new Promise<void>(resolve => {
			let entered = 0;
			release = () => {
				entered += 1;
				if (entered === 2) resolve();
			};
		});
		const readerBody = async () => {
			inFlight += 1;
			peakReaders = Math.max(peakReaders, inFlight);
			release();
			await bothEntered;
			inFlight -= 1;
			return { changedFiles: [], label: "reader", exitCode: 0 };
		};
		const writerBody = async () => {
			inFlight += 1;
			peakWriters = Math.max(peakWriters, inFlight);
			await Bun.sleep(10);
			inFlight -= 1;
			return { changedFiles: [], label: "writer", exitCode: 0 };
		};
		const readers = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [phase("scoutA", { produce: readerBody }), phase("scoutB", { produce: readerBody })],
		});
		expect(readers.status).toBe("accepted");
		expect(peakReaders).toBe(2);

		inFlight = 0;
		const writers = await runGraph({
			workflowId: "wf2",
			runDir,
			workspace: root,
			phases: [
				phase("buildA", { writes: true, produce: writerBody }),
				phase("buildB", { writes: true, produce: writerBody }),
			],
		});
		expect(writers.status).toBe("accepted");
		expect(peakWriters).toBe(1);
		expect(writers.peakConcurrentWriters).toBe(1);
	});

	it("routes a coherent rejection back to the named producer and re-runs its closure", async () => {
		await makeDirs();
		const dispatched: string[] = [];
		let planRuns = 0;
		let reviewRuns = 0;
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [
				phase("plan", {
					writes: true,
					produce: async () => {
						planRuns += 1;
						dispatched.push("plan");
						return { changedFiles: [], label: `plan-${planRuns}`, exitCode: 0, summary: `v${planRuns}` };
					},
				}),
				phase("build", {
					dependsOn: ["plan"],
					inputs: ["plan"],
					writes: true,
					produce: async () => {
						dispatched.push("build");
						return { changedFiles: [], label: "build", exitCode: 0 };
					},
				}),
				phase("review", {
					dependsOn: ["build"],
					onReject: { to: "plan", maxRevisions: 1 },
					produce: async () => {
						reviewRuns += 1;
						dispatched.push("review");
						return { changedFiles: [], label: "review", exitCode: 0 };
					},
					review: async () => ({
						approved: reviewRuns > 1,
						blockers: reviewRuns > 1 ? [] : ["plan is wrong"],
						findings: [],
					}),
				}),
			],
		});
		expect(result.status).toBe("accepted");
		expect(planRuns).toBe(2);
		expect(dispatched).toEqual(["plan", "build", "review", "plan", "build", "review"]);
		expect(result.phases.find(outcome => outcome.phase === "review")?.status).toBe("accepted");
	});

	it("stops revising at the declared budget", async () => {
		await makeDirs();
		let planRuns = 0;
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: [
				phase("plan", {
					produce: async () => {
						planRuns += 1;
						return { changedFiles: [], label: "plan", exitCode: 0 };
					},
				}),
				phase("review", {
					dependsOn: ["plan"],
					onReject: { to: "plan", maxRevisions: 2 },
					review: async () => ({ approved: false, blockers: ["never happy"], findings: [] }),
				}),
			],
		});
		expect(result.status).toBe("failed");
		expect(planRuns).toBe(3);
	});

	it("refuses a revision route that is not a transitive dependency", async () => {
		await makeDirs();
		await expect(
			runGraph({
				workflowId: "wf",
				runDir,
				workspace: root,
				phases: [
					phase("plan"),
					phase("aside"),
					phase("review", { dependsOn: ["plan"], onReject: { to: "aside", maxRevisions: 1 } }),
				],
			}),
		).rejects.toThrow('cannot revise "aside": not a transitive dependency');
	});
});
