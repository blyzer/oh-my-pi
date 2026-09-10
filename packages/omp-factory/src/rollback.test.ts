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
import { nativeWriteGuard } from "./write-guard";

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

function reachingWorkflow(): { workflow: ReturnType<typeof loadWorkflowConfig>; attempts: number[] } {
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
					await fs.writeFile(path.join(workspace, "src", "ok.ts"), "export const ok = 1;\n");
				}
				const changedFiles = await captureTouchedSince(state);
				return { changedFiles, declaredArtifacts: changedFiles, label: `c${attempt}`, exitCode: 0 };
			},
		},
	);
	return { workflow, attempts };
}

describe("what a rejected attempt leaves behind", () => {
	it("rolls the protected write back and lets the clean attempt stand", async () => {
		const repo = await makeRepo();
		const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-launder-run-"));
		tempDirs.push(runDir);
		const { workflow, attempts } = reachingWorkflow();

		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: repo,
			protectedGlobs: workflow.protectedGlobs,
			phases: workflow.phases,
			writeGuard: nativeWriteGuard(),
		});

		expect(attempts).toEqual([1, 2]);
		const build = result.phases.find(outcome => outcome.phase === "build");
		expect(build?.evidence[0]).toContain(".omp/adw/w.yml");
		expect(build?.evidence[0]).toContain("rolled back");
		// Undone, not merely refused: the tree must not keep what nothing
		// accepted, and the next attempt must not inherit it.
		expect(await fs.exists(path.join(repo, ".omp", "adw", "w.yml"))).toBeFalse();
		// The second attempt stayed in scope, so it stands on a clean tree.
		expect(result.status).toBe("accepted");
		expect(await fs.exists(path.join(repo, "src", "ok.ts"))).toBeTrue();
	});

	it("without a guard, still refuses the second attempt for the first's write", async () => {
		const repo = await makeRepo();
		const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-launder-run-"));
		tempDirs.push(runDir);
		const { workflow, attempts } = reachingWorkflow();

		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: repo,
			protectedGlobs: workflow.protectedGlobs,
			phases: workflow.phases,
			allowUnguardedWrites: true,
		});

		expect(attempts).toEqual([1, 2]);
		expect(result.status).toBe("failed");
		const build = result.phases.find(outcome => outcome.phase === "build");
		// Nothing rolled it back, so the entry-state baseline is the only
		// thing standing between attempt 1's protected write and acceptance.
		expect(build?.evidence.at(-1)).toContain(".omp/adw/w.yml");
		expect(await fs.exists(path.join(repo, ".omp", "adw", "w.yml"))).toBeTrue();
	});
});
