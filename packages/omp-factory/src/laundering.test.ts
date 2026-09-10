/**
 * A rejected attempt must not launder its own violations.
 *
 * The workspace survives across attempts, so a change-set measured from
 * inside attempt 2 starts after attempt 1's writes. Attempt 1's protected
 * file is then invisible to the very check that rejected attempt 1, and the
 * phase is accepted with that file still on disk.
 */
import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { captureTouchedSince } from "./capture";
import { runGraph } from "./graph";
import { loadWorkflowConfig } from "./workflow-config";

const tempDirs: string[] = [];

async function runGit(repo: string, args: string[]): Promise<void> {
	const proc = Bun.spawn(["git", ...args], { cwd: repo, stdout: "pipe", stderr: "pipe" });
	const [stderr, exitCode] = await Promise.all([new Response(proc.stderr).text(), proc.exited]);
	if ((exitCode ?? 0) !== 0) throw new Error(stderr.trim() || `git ${args.join(" ")} failed`);
}

async function makeRepo(): Promise<string> {
	const repo = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-launder-"));
	tempDirs.push(repo);
	await runGit(repo, ["init", "-q", "-b", "main"]);
	await runGit(repo, ["config", "user.email", "test@example.com"]);
	await runGit(repo, ["config", "user.name", "Test User"]);
	await runGit(repo, ["config", "maintenance.auto", "false"]);
	await fs.mkdir(path.join(repo, "src"), { recursive: true });
	await fs.writeFile(path.join(repo, "src", "base.ts"), "export const base = 1;\n");
	await runGit(repo, ["add", "."]);
	await runGit(repo, ["commit", "-q", "-m", "base"]);
	return repo;
}

afterEach(async () => {
	await Promise.all(tempDirs.splice(0).map(dir => fs.rm(dir, { recursive: true, force: true })));
});

describe("scope checking across attempts", () => {
	it("still sees a protected write from an earlier attempt of the same phase", async () => {
		const repo = await makeRepo();
		const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-launder-run-"));
		tempDirs.push(runDir);
		const attempts: number[] = [];

		const workflow = loadWorkflowConfig(
			["name: w", "maxAttempts: 2", "phases:", "  - name: build", "    owner: task", ""].join("\n"),
			{
				runAgent: async ({ attempt, entryState, workspace }) => {
					attempts.push(attempt);
					const state = await entryState();
					if (attempt === 1) {
						// Rewriting the workflow that judges you: always protected.
						await fs.mkdir(path.join(workspace, ".omp", "adw"), { recursive: true });
						await fs.writeFile(path.join(workspace, ".omp", "adw", "w.yml"), "name: mine\n");
					} else {
						// A clean second attempt — which must not clear the first.
						await fs.writeFile(path.join(workspace, "src", "ok.ts"), "export const ok = 1;\n");
					}
					const changedFiles = await captureTouchedSince(state);
					return { changedFiles, declaredArtifacts: changedFiles, label: `c${attempt}`, exitCode: 0 };
				},
			},
		);

		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: repo,
			protectedGlobs: workflow.protectedGlobs,
			phases: workflow.phases,
		});

		expect(attempts).toEqual([1, 2]);
		expect(result.status).toBe("failed");
		const build = result.phases.find(outcome => outcome.phase === "build");
		expect(build?.status).toBe("rejected");
		// Both attempts are rejected for the same file: the second attempt
		// inherits the first's writes because it inherits its workspace.
		expect(build?.evidence.at(-1)).toContain(".omp/adw/w.yml");
	});
});
