import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { replay } from "./ledger";
import { runWorkflow } from "./workflow";

let root = "";
let runDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-workflow-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-workflow-run-"));
}

afterEach(async () => {
	if (root) await fs.rm(root, { recursive: true, force: true });
	if (runDir) await fs.rm(runDir, { recursive: true, force: true });
	root = "";
	runDir = "";
});

describe("runWorkflow", () => {
	it("accepts a clean candidate and records version 1 in the ledger", async () => {
		await makeDirs();
		await Bun.write(path.join(root, "out.txt"), "DONE");
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [{ type: "file_contains", file: "out.txt", marker: "DONE" }],
			maxAttempts: 2,
			produce: async () => ({ changedFiles: ["out.txt"], label: "c1", exitCode: 0, output: "ok" }),
		});
		expect(result.status).toBe("accepted");
		const { projection } = await replay(runDir);
		expect(projection.status).toBe("accepted");
		expect(projection.phases.build).toMatchObject({ status: "accepted", attempts: 1, acceptedVersion: 1 });
	});

	it("corrects a scope violation with the next attempt", async () => {
		await makeDirs();
		let calls = 0;
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["src/**"],
			assertions: [],
			maxAttempts: 3,
			produce: async () => {
				calls += 1;
				return calls === 1
					? { changedFiles: ["README.md"], label: "stray", exitCode: 0 }
					: { changedFiles: ["src/a.ts"], label: "fixed", exitCode: 0 };
			},
		});
		expect(result.status).toBe("accepted");
		expect(result.attempts).toBe(2);
		expect(result.evidence[0]).toContain("scope violation");
	});

	it("rejects on builder nonzero exit without claiming success", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 1,
			produce: async () => ({ changedFiles: [], label: "crashed", exitCode: 1, output: "boom" }),
		});
		expect(result.status).toBe("rejected");
		const { projection } = await replay(runDir);
		expect(projection.status).toBe("failed");
		expect(projection.phases.build?.status).toBe("failed");
	});

	it("a gate failure plus an approving reviewer still rejects", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [{ type: "file_contains", file: "missing.txt", marker: "x" }],
			maxAttempts: 1,
			produce: async () => ({ changedFiles: [], label: "c1", exitCode: 0 }),
			review: async () => ({ approved: true, blockers: [], findings: [] }),
		});
		expect(result.status).toBe("rejected");
		expect(result.evidence[0]).toContain("missing artifact");
	});

	it("rejects in-scope writes the builder did not declare", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 1,
			produce: async () => ({
				changedFiles: ["src/a.ts", "src/sneaky.ts"],
				declaredArtifacts: ["src/a.ts"],
				label: "c1",
				exitCode: 0,
			}),
		});
		expect(result.status).toBe("rejected");
		expect(result.evidence[0]).toContain("undeclared writes: src/sneaky.ts");
	});

	it("rejects declared artifacts absent from the captured change-set", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 1,
			produce: async () => ({
				changedFiles: ["src/a.ts"],
				declaredArtifacts: ["src/a.ts", "src/phantom.ts"],
				label: "c1",
				exitCode: 0,
			}),
		});
		expect(result.status).toBe("rejected");
		expect(result.evidence[0]).toContain("claimed artifacts absent: src/phantom.ts");
	});

	it("accepts when the declaration matches the captured change-set", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["src/**"],
			assertions: [],
			maxAttempts: 1,
			produce: async () => ({
				changedFiles: ["src/a.ts"],
				declaredArtifacts: ["src/a.ts"],
				label: "c1",
				exitCode: 0,
			}),
		});
		expect(result.status).toBe("accepted");
	});

	it("lands a terminal ledger when the producer throws", async () => {
		await makeDirs();
		const result = await runWorkflow({
			workflowId: "wf-1",
			phase: "build",
			runDir,
			workspace: root,
			scope: ["**"],
			assertions: [],
			maxAttempts: 2,
			produce: async () => {
				throw new Error("spawn unavailable");
			},
		});
		expect(result.status).toBe("rejected");
		expect(result.evidence[0]).toContain("producer threw: spawn unavailable");
		const { projection } = await replay(runDir);
		expect(projection.status).toBe("failed");
		expect(projection.phases.build?.status).toBe("failed");
	});

	it("journals an accepted candidate through to committed", async () => {
		await makeDirs();
		const journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-wf-journal-"));
		try {
			await Bun.write(path.join(root, "out.txt"), "DONE");
			const result = await runWorkflow({
				workflowId: "wf-1",
				phase: "build",
				runDir,
				workspace: root,
				scope: ["**"],
				assertions: [],
				maxAttempts: 1,
				integrate: { journalDir, base: "base-1" },
				produce: async () => ({ changedFiles: ["out.txt"], label: "c1", exitCode: 0 }),
			});
			expect(result.status).toBe("accepted");
			const stored = (await Bun.file(path.join(journalDir, "integration.json")).json()) as {
				status: string;
			};
			expect(stored.status).toBe("committed");
			const { projection } = await replay(runDir);
			expect(projection.phases.build?.integration).toBe("committed");
		} finally {
			await fs.rm(journalDir, { recursive: true, force: true });
		}
	});

	it("fails closed when the candidate deletes files", async () => {
		await makeDirs();
		const journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-wf-journal-"));
		try {
			const result = await runWorkflow({
				workflowId: "wf-1",
				phase: "build",
				runDir,
				workspace: root,
				scope: ["**"],
				assertions: [],
				maxAttempts: 1,
				integrate: { journalDir, base: "base-1" },
				produce: async () => ({ changedFiles: ["gone.txt"], label: "c1", exitCode: 0 }),
			});
			expect(result.status).toBe("rejected");
			expect(result.evidence[0]).toContain("deleted files unsupported");
		} finally {
			await fs.rm(journalDir, { recursive: true, force: true });
		}
	});
});
