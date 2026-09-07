import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { $ } from "bun";
import { afterEach, describe, expect, it } from "bun:test";
import { mapWithLimit, runCodePhase, settleIsolation } from "@oh-my-pi/pi-coding-agent/adw/runner";
import type { AdwPhaseConfig } from "@oh-my-pi/pi-coding-agent/adw/types";
import { prepareIsolationContext } from "@oh-my-pi/pi-coding-agent/task/isolation-runner";
import { cleanupIsolation, ensureIsolation } from "@oh-my-pi/pi-coding-agent/task/worktree";

const roots: string[] = [];

function tempDir(label: string): string {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), `adw-${label}-`));
	roots.push(dir);
	return dir;
}

afterEach(() => {
	for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

function code(command: string, extra: Partial<AdwPhaseConfig> = {}): AdwPhaseConfig {
	return { name: "p", kind: "code", owner: "sh", command, ...extra };
}

describe("runCodePhase", () => {
	it("passes on exit 0 and carries the output", async () => {
		const result = await runCodePhase(code("echo hello"), tempDir("ok"), undefined);
		expect(result.ok).toBe(true);
		expect(result.summary).toContain("hello");
	});

	it("fails on a non-zero exit and reports the code with the log tail", async () => {
		const result = await runCodePhase(code("echo boom >&2; exit 3"), tempDir("fail"), undefined);
		expect(result.ok).toBe(false);
		expect(result.summary).toContain("exited 3");
		expect(result.summary).toContain("boom");
	});

	it("runs in the directory it is given", async () => {
		const dir = tempDir("cwd");
		fs.writeFileSync(path.join(dir, "marker.txt"), "here");
		expect((await runCodePhase(code("cat marker.txt"), dir, undefined)).summary).toContain("here");
	});

	it("reports a timed-out command as killed, never as its exit code", async () => {
		// Integration by necessity: the behavior under test IS the real deadline.
		// `allowAbort` makes a timeout return normally, and the command can still
		// exit 0 in the race between the deadline firing and the kill landing —
		// reading exitCode alone wrote a killed suite into the trace as a pass.
		const result = await runCodePhase(code("sleep 30; exit 0", { timeoutMs: 200 }), tempDir("kill"), undefined);
		expect(result.ok).toBe(false);
		expect(result.summary).toContain("killed");
	});

	it("fails a command whose signal was already aborted rather than reporting a build error", async () => {
		const controller = new AbortController();
		controller.abort(new Error("interrupted"));
		const result = await runCodePhase(code("sleep 30"), tempDir("abort"), controller.signal);
		expect(result.ok).toBe(false);
		// Never an exit-code story: a cancelled run must not read as a red suite.
		expect(result.summary).not.toContain("exited 0");
	});
});

describe("mapWithLimit", () => {
	it("keeps results in input order regardless of completion order", async () => {
		const gates = [0, 1, 2, 3].map(() => Promise.withResolvers<void>());
		const pending = mapWithLimit([0, 1, 2, 3], 4, async (_item, index) => {
			await gates[index]?.promise;
			return index;
		});
		// Finish in reverse; input order must still decide the result order.
		for (const gate of [...gates].reverse()) gate.resolve();
		expect(await pending).toEqual([0, 1, 2, 3]);
	});

	it("never exceeds the ceiling", async () => {
		const items = Array.from({ length: 9 }, (_, index) => index);
		const gates = items.map(() => Promise.withResolvers<void>());
		let live = 0;
		let peak = 0;
		const saturated = Promise.withResolvers<void>();

		const pending = mapWithLimit(items, 2, async (_item, index) => {
			live++;
			peak = Math.max(peak, live);
			if (live === 2) saturated.resolve();
			await gates[index]?.promise;
			live--;
			return index;
		});

		// Wait for the pool to fill, not for a duration.
		await saturated.promise;
		expect(peak).toBe(2);
		for (const gate of gates) gate.resolve();
		expect(await pending).toHaveLength(9);
		expect(peak).toBe(2);
	});

	it("runs everything when the ceiling exceeds the item count", async () => {
		expect(await mapWithLimit([1, 2, 3], 99, async value => value * 2)).toEqual([2, 4, 6]);
	});
});

describe("settleIsolation", () => {
	async function repoWithSandbox(label: string) {
		const repoRoot = tempDir(label);
		await $`git init -q`.cwd(repoRoot).quiet();
		await $`git config user.email a@b.c`.cwd(repoRoot).quiet();
		await $`git config user.name t`.cwd(repoRoot).quiet();
		fs.writeFileSync(path.join(repoRoot, "tracked.txt"), "original\n");
		await $`git add -A`.cwd(repoRoot).quiet();
		await $`git commit -qm init`.cwd(repoRoot).quiet();

		const context = await prepareIsolationContext(repoRoot);
		const handle = await ensureIsolation(repoRoot, `adw-test-${label}`, undefined);
		return { repoRoot, handle, isolation: { handle, context } };
	}

	it("leaves the checkout untouched when the run was rejected, keeping the patch", async () => {
		const { repoRoot, handle, isolation } = await repoWithSandbox("reject");
		try {
			fs.writeFileSync(path.join(handle.mergedDir, "tracked.txt"), "contaminated\n");
			const outcome = await settleIsolation(isolation, false, tempDir("reject-run"), "adw-x");

			expect(outcome.hadChanges).toBe(true);
			expect(outcome.applied).toBe(false);
			expect(fs.readFileSync(path.join(repoRoot, "tracked.txt"), "utf8")).toBe("original\n");
			expect(fs.readFileSync(outcome.patchPath as string, "utf8")).toContain("contaminated");
		} finally {
			await cleanupIsolation(handle);
		}
	});

	it("applies the diff to the checkout when the run was accepted", async () => {
		const { repoRoot, handle, isolation } = await repoWithSandbox("accept");
		try {
			fs.writeFileSync(path.join(handle.mergedDir, "tracked.txt"), "original\nfrom-sandbox\n");
			fs.writeFileSync(path.join(handle.mergedDir, "added.txt"), "new\n");
			const outcome = await settleIsolation(isolation, true, tempDir("accept-run"), "adw-y");

			expect(outcome.applied).toBe(true);
			expect(fs.readFileSync(path.join(repoRoot, "tracked.txt"), "utf8")).toContain("from-sandbox");
			expect(fs.readFileSync(path.join(repoRoot, "added.txt"), "utf8")).toBe("new\n");
		} finally {
			await cleanupIsolation(handle);
		}
	});

	it("reports a conflict and preserves the patch when the checkout moved underneath", async () => {
		const { repoRoot, handle, isolation } = await repoWithSandbox("conflict");
		try {
			fs.writeFileSync(path.join(handle.mergedDir, "tracked.txt"), "sandbox edit\n");
			// The operator edited the same line while the run was in flight.
			fs.writeFileSync(path.join(repoRoot, "tracked.txt"), "human edit\n");
			const outcome = await settleIsolation(isolation, true, tempDir("conflict-run"), "adw-z");

			expect(outcome.applied).toBe(false);
			expect(outcome.conflict).toBeTruthy();
			// Neither half-applied nor lost.
			expect(fs.readFileSync(path.join(repoRoot, "tracked.txt"), "utf8")).toBe("human edit\n");
			expect(fs.readFileSync(outcome.patchPath as string, "utf8")).toContain("sandbox edit");
		} finally {
			await cleanupIsolation(handle);
		}
	});

	it("reports no changes for an untouched sandbox without writing a patch", async () => {
		const { handle, isolation } = await repoWithSandbox("clean");
		try {
			const outcome = await settleIsolation(isolation, true, tempDir("clean-run"), "adw-w");
			expect(outcome.hadChanges).toBe(false);
			expect(outcome.applied).toBe(true);
			expect(outcome.patchPath).toBeUndefined();
		} finally {
			await cleanupIsolation(handle);
		}
	});
});

describe("settleIsolation nested repositories", () => {
	it("applies a nested repo's diff after the root landed", async () => {
		const repoRoot = tempDir("nested");
		await $`git init -q`.cwd(repoRoot).quiet();
		await $`git config user.email a@b.c`.cwd(repoRoot).quiet();
		await $`git config user.name t`.cwd(repoRoot).quiet();
		fs.writeFileSync(path.join(repoRoot, "root.txt"), "root\n");
		await $`git add -A`.cwd(repoRoot).quiet();
		await $`git commit -qm init`.cwd(repoRoot).quiet();

		// A vendored checkout: its own git repo living inside the parent tree.
		const nested = path.join(repoRoot, "vendor");
		fs.mkdirSync(nested);
		await $`git init -q`.cwd(nested).quiet();
		await $`git config user.email a@b.c`.cwd(nested).quiet();
		await $`git config user.name t`.cwd(nested).quiet();
		fs.writeFileSync(path.join(nested, "lib.txt"), "v1\n");
		await $`git add -A`.cwd(nested).quiet();
		await $`git commit -qm init`.cwd(nested).quiet();

		const context = await prepareIsolationContext(repoRoot);
		const handle = await ensureIsolation(repoRoot, "adw-test-nested", undefined);
		try {
			fs.writeFileSync(path.join(handle.mergedDir, "root.txt"), "root\nchanged\n");
			fs.writeFileSync(path.join(handle.mergedDir, "vendor", "lib.txt"), "v2\n");
			const outcome = await settleIsolation({ handle, context }, true, tempDir("nested-run"), "adw-n");

			expect(outcome.applied).toBe(true);
			expect(outcome.nestedPatches).toBeGreaterThan(0);
			expect(outcome.nestedApplied).toBe(true);
			expect(outcome.nestedWarnings).toEqual([]);
			expect(fs.readFileSync(path.join(nested, "lib.txt"), "utf8")).toBe("v2\n");
		} finally {
			await cleanupIsolation(handle);
		}
	});
});

describe("runCodePhase signal reporting", () => {
	it("names the signal instead of leaving the shell's 128+N code raw", async () => {
		// A host shutting down sends SIGTERM; `sh` reports its dead child as 143.
		// Left raw it reads to the next agent as a failing build.
		const result = await runCodePhase(code(`sh -c 'kill -TERM $$'`), tempDir("sigterm"), undefined);
		expect(result.ok).toBe(false);
		expect(result.summary).toContain("SIGTERM");
		expect(result.summary).not.toContain("exited 143");
	});

	it("names SIGKILL too", async () => {
		const result = await runCodePhase(code(`sh -c 'kill -KILL $$'`), tempDir("sigkill"), undefined);
		expect(result.ok).toBe(false);
		expect(result.summary).toContain("SIGKILL");
	});

	it("still reports an ordinary non-zero exit as an exit", async () => {
		const result = await runCodePhase(code("exit 3"), tempDir("plain"), undefined);
		expect(result.summary).toContain("exited 3");
		expect(result.summary).not.toContain("killed");
	});

	it("calls a cancelled command cancelled, not timed out", async () => {
		const controller = new AbortController();
		controller.abort(new Error("interrupted"));
		const result = await runCodePhase(code("sleep 30"), tempDir("cancelled"), controller.signal);
		expect(result.ok).toBe(false);
		expect(result.summary).not.toContain("timed out");
	});
});
