import { $ } from "bun";
import { describe, expect, it } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { buildPhasePrompt } from "../../src/adw/prompt";
import { collectReviewDiff } from "../../src/adw/runner";
import type { AdwPhaseConfig } from "../../src/adw/types";

async function repo(): Promise<string> {
	const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "review-diff-"));
	await $`git init -q ${cwd}`.quiet();
	fs.writeFileSync(path.join(cwd, "a.ts"), "export const x = 1;\n");
	await $`git -C ${cwd} add -A`.quiet();
	await $`git -C ${cwd} -c user.email=t@t -c user.name=t commit -qm base`.quiet();
	return cwd;
}

const reviewPhase = { name: "review", kind: "agent", owner: "task" } as AdwPhaseConfig;

describe("collectReviewDiff", () => {
	it("reports modified and untracked paths with the real patch", async () => {
		// A reviewer given only the writer's envelope can confirm the account is
		// coherent, never that it is true. The undeclared file is the case that
		// matters: nothing in the envelope mentions it.
		const cwd = await repo();
		fs.writeFileSync(path.join(cwd, "a.ts"), "export const x = 2;\n");
		fs.writeFileSync(path.join(cwd, "undeclared.ts"), "export const y = 9;\n");

		const diff = await collectReviewDiff(cwd);
		expect(diff?.paths).toEqual(["a.ts", "undeclared.ts"]);
		expect(diff?.patch).toContain("export const x = 2");
		expect(diff?.truncated).toBeFalse();
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("reports an unchanged tree as empty rather than as a missing diff", async () => {
		// Distinct from failure: "nothing changed" is a fact the reviewer must
		// act on, and it must not read as "the diff was unavailable".
		const cwd = await repo();
		const diff = await collectReviewDiff(cwd);
		expect(diff?.paths).toEqual([]);
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("returns undefined outside a repository instead of throwing", async () => {
		// A missing tool must not turn into a failed acceptance: the review runs
		// weaker, it does not fail closed on infrastructure.
		const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "not-a-repo-"));
		expect(await collectReviewDiff(cwd)).toBeUndefined();
		fs.rmSync(cwd, { recursive: true, force: true });
	});
});

describe("review diff in the phase prompt", () => {
	it("tells the reviewer an empty tree has nothing to approve", () => {
		// Rendering an empty section would read as "all clear" to a model that
		// is being asked to approve something.
		const prompt = buildPhasePrompt({
			request: "r",
			phase: reviewPhase,
			attempt: 1,
			reviewDiff: { paths: [], patch: "", truncated: false },
		});
		expect(prompt).toContain("nothing to approve");
	});

	it("marks a truncated diff so it is not approved unread", () => {
		// Approving what was cut off is the failure mode; the reviewer is told
		// to block on it instead.
		const prompt = buildPhasePrompt({
			request: "r",
			phase: reviewPhase,
			attempt: 1,
			reviewDiff: { paths: ["a.ts"], patch: "@@ -1 +1 @@", truncated: true },
		});
		expect(prompt).toContain("TRUNCATED");
		expect(prompt).toContain("blocking");
	});

	it("omits the section entirely when there is no diff", () => {
		// A builder phase would only be told what it just wrote.
		const prompt = buildPhasePrompt({ request: "r", phase: reviewPhase, attempt: 1 });
		expect(prompt).not.toContain("Change-set under review");
	});
});
