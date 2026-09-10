import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { evaluateFileAssertions, runGateCommand } from "./gates";
import { runAttemptLoop } from "./loop";
import { verifyScope } from "./scope";

let workspace = "";

async function makeWorkspace(): Promise<string> {
	workspace = await fs.mkdtemp(path.join(os.tmpdir(), "factory-boundary-"));
	return workspace;
}

afterEach(async () => {
	if (workspace) await fs.rm(workspace, { recursive: true, force: true });
	workspace = "";
});

describe("verifyScope", () => {
	it("accepts change-sets inside the declared scope", () => {
		const verdict = verifyScope(["src/foo/a.ts", "tests/foo/a.test.ts"], ["src/foo/**", "tests/foo/**"]);
		expect(verdict).toEqual({ ok: true, violations: [] });
	});

	it("rejects a file outside scope regardless of claimed intent", () => {
		const verdict = verifyScope(["src/foo/a.ts", "README.md"], ["src/foo/**"]);
		expect(verdict.ok).toBeFalse();
		expect(verdict.violations).toEqual(["README.md"]);
	});
});

describe("evaluateFileAssertions", () => {
	it("WI-0024: contradictory contains/not-contains can never accept", async () => {
		const root = await makeWorkspace();
		await Bun.write(path.join(root, "marker.txt"), "anything");
		const report = await evaluateFileAssertions(
			[
				{ type: "file_contains", file: "marker.txt", marker: "DONE" },
				{ type: "file_not_contains", file: "marker.txt", marker: "DONE" },
			],
			root,
		);
		expect(report.passed).toBeFalse();
		expect(report.failures.length).toBeGreaterThan(0);
	});

	it("fails closed on unknown assertion types", async () => {
		const root = await makeWorkspace();
		const report = await evaluateFileAssertions([{ type: "file_smells_good", file: "a.txt" }], root);
		expect(report.passed).toBeFalse();
		expect(report.failures[0]).toContain("unknown assertion type");
	});

	it("fails closed on missing artifacts", async () => {
		const root = await makeWorkspace();
		const report = await evaluateFileAssertions([{ type: "file_contains", file: "absent.txt", marker: "x" }], root);
		expect(report.passed).toBeFalse();
		expect(report.failures[0]).toContain("missing artifact");
	});
});

describe("runGateCommand", () => {
	it("treats exit 0 as pass", async () => {
		const verdict = await runGateCommand(["true"], "/tmp");
		expect(verdict.exitCode).toBe(0);
	});

	it("treats nonzero exit as failure with captured output", async () => {
		const verdict = await runGateCommand(["sh", "-c", "echo gate-evidence; exit 3"], "/tmp");
		expect(verdict.exitCode).toBe(3);
		expect(verdict.output).toContain("gate-evidence");
	});
});

describe("runAttemptLoop", () => {
	it("returns gate failure evidence to the next attempt and accepts the fix", async () => {
		const seen: Array<string | undefined> = [];
		const result = await runAttemptLoop({
			maxAttempts: 3,
			produce: async (_attempt, evidence) => {
				seen.push(evidence);
				return { changedFiles: [], label: "candidate" };
			},
			verify: async () => {
				if (seen.length === 1) return { accepted: false, evidence: "marker missing" };
				return { accepted: true };
			},
		});
		expect(result.status).toBe("accepted");
		expect(result.attempts).toBe(2);
		expect(seen[1]).toContain("marker missing");
		expect(result.evidence).toHaveLength(1);
	});

	it("exhausts a finite budget and rejects with the full evidence trail", async () => {
		let productions = 0;
		const result = await runAttemptLoop({
			maxAttempts: 2,
			produce: async () => {
				productions += 1;
				return { changedFiles: [], label: `c${productions}` };
			},
			verify: async () => ({ accepted: false, evidence: "still red" }),
		});
		expect(result.status).toBe("rejected");
		expect(productions).toBe(2);
		expect(result.evidence).toHaveLength(2);
	});

	it("rejects a non-positive budget without producing anything", async () => {
		let productions = 0;
		const result = await runAttemptLoop({
			maxAttempts: 0,
			produce: async () => {
				productions += 1;
				return { changedFiles: [], label: "never" };
			},
			verify: async () => ({ accepted: true }),
		});
		expect(result.status).toBe("rejected");
		expect(productions).toBe(0);
	});
});
