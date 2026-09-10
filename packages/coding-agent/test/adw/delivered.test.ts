/**
 * I14 against the shipped driver: verifying the candidate is not verifying
 * what gets delivered.
 *
 * A writer is checked inside its own workspace. The patch lands somewhere
 * else. When a change-set is valid alone and broken once it is on the shared
 * root, nothing in the candidate path can see it — each workspace was
 * genuinely fine.
 *
 * These drive `integrateAccepted` directly, because that is the boundary
 * where APPLIED becomes COMMITTED and where the revert has to live.
 */
import { describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { integrateAccepted, type IntegrationRecord, writeRunState } from "../../src/adw/integration";
import { captureBaseline, captureDeltaPatch } from "../../src/task/worktree";

async function git(cwd: string, ...args: string[]): Promise<void> {
	const proc = Bun.spawn(["git", ...args], { cwd, stdout: "ignore", stderr: "pipe" });
	const [stderr, code] = await Promise.all([new Response(proc.stderr).text(), proc.exited]);
	if (code !== 0) throw new Error(stderr.trim() || `git ${args.join(" ")} failed`);
}

/** A repo holding one committed file, plus a run directory beside it. */
async function scratch(): Promise<{ root: string; runDir: string }> {
	const root = await fs.mkdtemp(path.join(os.tmpdir(), "adw-delivered-"));
	const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "adw-delivered-run-"));
	await git(root, "init", "-q", "-b", "main");
	await git(root, "config", "user.email", "t@e.com");
	await git(root, "config", "user.name", "T");
	await git(root, "config", "maintenance.auto", "false");
	await Bun.write(path.join(root, "base.txt"), "original\n");
	await git(root, "add", "-A");
	await git(root, "commit", "-qm", "base");
	return { root, runDir };
}

/** Capture a patch for whatever `mutate` does, without landing it. */
async function prepared(root: string, mutate: () => Promise<void>): Promise<IntegrationRecord> {
	const baseline = await captureBaseline(root);
	await mutate();
	const delta = await captureDeltaPatch(root, baseline);
	// Put the tree back: the record now describes a change that has NOT been
	// applied, which is the state integrateAccepted expects.
	await git(root, "checkout", "-q", "--", ".");
	await fs.rm(path.join(root, "added.txt"), { force: true });
	return { phase: "build", fromSeq: 0, delta, status: "prepared" };
}

describe("delivered-tree verification", () => {
	it("reverts the landing when the delivered tree fails its check", async () => {
		const { root, runDir } = await scratch();
		const record = await prepared(root, async () => {
			await Bun.write(path.join(root, "base.txt"), "landed\n");
			await Bun.write(path.join(root, "added.txt"), "new\n");
		});
		await writeRunState(path.join(runDir, "integration.json"), record);

		await expect(
			integrateAccepted(root, runDir, record, async () => ({ ok: false, evidence: "suite is red" })),
		).rejects.toThrow(/suite is red; landing reverted/);

		// The revert is what makes this fail closed: refusing to call it
		// delivered while leaving it on disk would be the worst of both.
		expect(await Bun.file(path.join(root, "base.txt")).text()).toBe("original\n");
		expect(await fs.exists(path.join(root, "added.txt"))).toBeFalse();

		const journal = (await Bun.file(path.join(runDir, "integration.json")).json()) as IntegrationRecord;
		expect(journal.status).toBe("rejected");

		await fs.rm(root, { recursive: true, force: true });
		await fs.rm(runDir, { recursive: true, force: true });
	});

	it("commits the landing when the delivered tree passes", async () => {
		const { root, runDir } = await scratch();
		const record = await prepared(root, async () => {
			await Bun.write(path.join(root, "base.txt"), "landed\n");
		});
		await writeRunState(path.join(runDir, "integration.json"), record);

		await integrateAccepted(root, runDir, record, async () => ({ ok: true }));

		expect(await Bun.file(path.join(root, "base.txt")).text()).toBe("landed\n");
		const journal = (await Bun.file(path.join(runDir, "integration.json")).json()) as IntegrationRecord;
		expect(journal.status).toBe("integrated");

		await fs.rm(root, { recursive: true, force: true });
		await fs.rm(runDir, { recursive: true, force: true });
	});

	it("lands unchanged when no delivered check is supplied", async () => {
		// A workflow with no code phase downstream of its writer declared no
		// deterministic check; applying one anyway would enforce a gate the
		// author never asked for.
		const { root, runDir } = await scratch();
		const record = await prepared(root, async () => {
			await Bun.write(path.join(root, "base.txt"), "landed\n");
		});
		await writeRunState(path.join(runDir, "integration.json"), record);

		await integrateAccepted(root, runDir, record);

		expect(await Bun.file(path.join(root, "base.txt")).text()).toBe("landed\n");
		await fs.rm(root, { recursive: true, force: true });
		await fs.rm(runDir, { recursive: true, force: true });
	});
});
