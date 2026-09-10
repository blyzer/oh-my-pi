import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { captureBaselineState, captureTouchedSince } from "./capture";

const tempDirs: string[] = [];

async function runGit(repo: string, args: string[]): Promise<void> {
	const proc = Bun.spawn(["git", ...args], { cwd: repo, stdout: "pipe", stderr: "pipe" });
	const [stderr, exitCode] = await Promise.all([new Response(proc.stderr).text(), proc.exited]);
	if ((exitCode ?? 0) !== 0) throw new Error(stderr.trim() || `git ${args.join(" ")} failed`);
}

async function makeGitRepo(): Promise<string> {
	const repo = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-capture-"));
	tempDirs.push(repo);
	await runGit(repo, ["init", "-q", "-b", "main"]);
	await runGit(repo, ["config", "user.email", "test@example.com"]);
	await runGit(repo, ["config", "user.name", "Test User"]);
	await fs.writeFile(path.join(repo, "base.txt"), "base\n");
	await runGit(repo, ["add", "."]);
	await runGit(repo, ["commit", "-q", "-m", "base"]);
	return repo;
}

afterEach(async () => {
	await Promise.all(tempDirs.splice(0).map(dir => fs.rm(dir, { recursive: true, force: true })));
});

describe("change capture", () => {
	it("reports staged, unstaged, and untracked files actually touched", async () => {
		const repo = await makeGitRepo();
		const state = await captureBaselineState(repo);
		await fs.writeFile(path.join(repo, "base.txt"), "unstaged\n");
		await fs.writeFile(path.join(repo, "staged.txt"), "staged\n");
		await runGit(repo, ["add", "staged.txt"]);
		await fs.writeFile(path.join(repo, "untracked.txt"), "new\n");
		await expect(captureTouchedSince(state)).resolves.toEqual(["base.txt", "staged.txt", "untracked.txt"]);
	});

	it("reports nothing touched on a quiet tree", async () => {
		const repo = await makeGitRepo();
		const state = await captureBaselineState(repo);
		await expect(captureTouchedSince(state)).resolves.toEqual([]);
	});

	it("refuses to baseline outside a git checkout", async () => {
		const plain = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-plain-"));
		tempDirs.push(plain);
		await expect(captureBaselineState(plain)).rejects.toThrow();
	});
});
