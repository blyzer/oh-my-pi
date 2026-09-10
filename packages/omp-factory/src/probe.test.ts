import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import factoryExtension, { buildProbeAssignment } from "./extension";

const tempDirs: string[] = [];

async function runGit(repo: string, args: string[]): Promise<void> {
	const proc = Bun.spawn(["git", ...args], { cwd: repo, stdout: "pipe", stderr: "pipe" });
	const [stderr, exitCode] = await Promise.all([new Response(proc.stderr).text(), proc.exited]);
	if ((exitCode ?? 0) !== 0) throw new Error(stderr.trim() || `git ${args.join(" ")} failed`);
}

async function makeGitRepo(): Promise<string> {
	const repo = await fs.mkdtemp(path.join(os.tmpdir(), "omp-factory-probe-"));
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

describe("buildProbeAssignment", () => {
	it("uses the operator assignment verbatim", () => {
		expect(buildProbeAssignment("  fix the retry helper  ")).toBe("fix the retry helper");
	});

	it("falls back to a deterministic probe when empty", () => {
		expect(buildProbeAssignment("   ")).toBe("Reply with exactly: FACTORY-PROBE-OK");
	});
});
async function ledgerDirOf(notices: Array<{ message: string }>): Promise<string | null> {
	const match = notices[0]?.message.match(/ledger=(\S+)/);
	return match?.[1] ?? null;
}

describe("/factory probe command", () => {
	it("spawns one managed builder in the session cwd and reports its result", async () => {
		const registrations = new Map<string, { handler: (args: string, ctx: never) => Promise<void> }>();
		const spawnOptions: Record<string, unknown>[] = [];
		const notices: Array<{ message: string; kind: string }> = [];
		const pi = {
			registerCommand: (name: string, def: never) => {
				registrations.set(name, def as never);
			},
			pi: {
				runSubprocess: async (options: Record<string, unknown>) => {
					spawnOptions.push(options);
					return { exitCode: 0, output: "FACTORY-PROBE-OK" };
				},
			},
		};
		const repo = await makeGitRepo();
		const ctx = {
			cwd: repo,
			modelRegistry: { marker: "session-registry" },
			ui: {
				notify: (message: string, kind: string) => {
					notices.push({ message, kind });
				},
			},
		};

		factoryExtension(pi as never);
		const command = registrations.get("factory");
		if (!command) throw new Error("/factory was not registered");
		await command.handler("do the thing", ctx as never);

		expect(spawnOptions).toHaveLength(1);
		const spawn = spawnOptions[0] as Record<string, unknown>;
		expect(spawn.cwd).toBe(repo);
		expect(spawn.task).toBe("do the thing");
		expect(spawn.modelRegistry).toBe(ctx.modelRegistry);
		expect(notices).toHaveLength(1);
		expect(notices[0]?.kind).toBe("info");
		expect(notices[0]?.message).toContain("accepted");
		expect(notices[0]?.message).toContain("ledger=");
		const ledger = await ledgerDirOf(notices);
		if (ledger) await fs.rm(ledger, { recursive: true, force: true });
	});

	it("surfaces builder failure to the operator instead of claiming success", async () => {
		const registrations = new Map<string, { handler: (args: string, ctx: never) => Promise<void> }>();
		const notices: Array<{ message: string; kind: string }> = [];
		const pi = {
			registerCommand: (name: string, def: never) => {
				registrations.set(name, def as never);
			},
			pi: {
				runSubprocess: async () => ({ exitCode: 1, output: "", error: "boom" }),
			},
		};
		const repo = await makeGitRepo();
		const ctx = {
			cwd: repo,
			modelRegistry: {},
			ui: {
				notify: (message: string, kind: string) => {
					notices.push({ message, kind });
				},
			},
		};

		factoryExtension(pi as never);
		const command = registrations.get("factory");
		if (!command) throw new Error("/factory was not registered");
		await command.handler("", ctx as never);

		expect(notices[0]?.kind).toBe("error");
		expect(notices[0]?.message).toContain("rejected");
		expect(notices[0]?.message).toContain("exit 1");
		const ledger = await ledgerDirOf(notices);
		if (ledger) await fs.rm(ledger, { recursive: true, force: true });
	});
});
