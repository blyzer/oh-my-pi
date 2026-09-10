import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { type GraphPhase, runGraph } from "./graph";
import { copyIsolation } from "./isolation";

let root = "";
let runDir = "";
let journalDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-iso-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-iso-run-"));
	journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-iso-journal-"));
	await Bun.write(path.join(root, "base.txt"), "base\n");
}

afterEach(async () => {
	for (const dir of [root, runDir, journalDir]) {
		if (dir) await fs.rm(dir, { recursive: true, force: true });
	}
	root = "";
	runDir = "";
	journalDir = "";
});

function writer(name: string, file: string, extra: Partial<GraphPhase> = {}): GraphPhase {
	return {
		name,
		scope: ["**"],
		assertions: [],
		maxAttempts: 1,
		writes: true,
		produce: async ({ workspace }) => {
			await Bun.write(path.join(workspace, file), `${name} wrote this\n`);
			return { changedFiles: [file], declaredArtifacts: [file], label: name, exitCode: 0 };
		},
		...extra,
	};
}

describe("isolated parallel writers", () => {
	it("gives each writer its own tree and lands both diffs on the shared root", async () => {
		await makeDirs();
		const seenWorkspaces = new Set<string>();
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			isolation: copyIsolation(),
			integrate: { journalDir, base: "iso-base" },
			phases: [
				writer("alpha", "alpha.txt", {
					produce: async ({ workspace }) => {
						seenWorkspaces.add(workspace);
						await Bun.write(path.join(workspace, "alpha.txt"), "alpha wrote this\n");
						return {
							changedFiles: ["alpha.txt"],
							declaredArtifacts: ["alpha.txt"],
							label: "alpha",
							exitCode: 0,
						};
					},
				}),
				writer("beta", "beta.txt", {
					produce: async ({ workspace }) => {
						seenWorkspaces.add(workspace);
						await Bun.write(path.join(workspace, "beta.txt"), "beta wrote this\n");
						return { changedFiles: ["beta.txt"], declaredArtifacts: ["beta.txt"], label: "beta", exitCode: 0 };
					},
				}),
			],
		});
		expect(result.status).toBe("accepted");
		// Two distinct sandboxes, neither of them the shared root.
		expect(seenWorkspaces.size).toBe(2);
		expect(seenWorkspaces.has(root)).toBeFalse();
		expect(result.peakConcurrentWriters).toBe(2);
		expect(await Bun.file(path.join(root, "alpha.txt")).text()).toBe("alpha wrote this\n");
		expect(await Bun.file(path.join(root, "beta.txt")).text()).toBe("beta wrote this\n");
	});

	it("keeps an unaccepted writer's work out of the shared root", async () => {
		await makeDirs();
		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			isolation: copyIsolation(),
			integrate: { journalDir, base: "iso-base" },
			phases: [
				writer("stray", "stray.txt", {
					scope: ["allowed/**"],
					produce: async ({ workspace }) => {
						await Bun.write(path.join(workspace, "stray.txt"), "out of scope\n");
						return {
							changedFiles: ["stray.txt"],
							declaredArtifacts: ["stray.txt"],
							label: "stray",
							exitCode: 0,
						};
					},
				}),
			],
		});
		expect(result.status).toBe("failed");
		expect(result.phases[0]?.evidence[0]).toContain("scope violation");
		await expect(Bun.file(path.join(root, "stray.txt")).text()).rejects.toThrow();
	});

	it("removes the sandbox once the phase settles", async () => {
		await makeDirs();
		let sandbox = "";
		await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			isolation: copyIsolation(),
			phases: [
				writer("alpha", "alpha.txt", {
					produce: async ({ workspace }) => {
						sandbox = workspace;
						return { changedFiles: [], label: "alpha", exitCode: 0 };
					},
				}),
			],
		});
		expect(sandbox).not.toBe("");
		await expect(fs.stat(sandbox)).rejects.toThrow();
	});
});
