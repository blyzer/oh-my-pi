import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { applyIntegration, commitIntegration, loadJournal, prepareIntegration, recoverIntegration } from "./integrate";

let root = "";
let journalDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-integrate-root-"));
	journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-integrate-journal-"));
	await Bun.write(path.join(root, "src", "keep.ts"), "original");
}

afterEach(async () => {
	if (root) await fs.rm(root, { recursive: true, force: true });
	if (journalDir) await fs.rm(journalDir, { recursive: true, force: true });
	root = "";
	journalDir = "";
});

describe("integration journal", () => {
	it("applies a prepared patch exactly once and commits", async () => {
		await makeDirs();
		const journal = await prepareIntegration(root, "base-1", {
			"src/keep.ts": "fixed",
			"src/new.ts": "added",
		});
		expect(journal.status).toBe("prepared");
		const applied = await applyIntegration(root, journalDir, journal);
		expect(applied.status).toBe("applied");
		expect(await Bun.file(path.join(root, "src/keep.ts")).text()).toBe("fixed");
		const committed = await commitIntegration(journalDir, applied);
		expect(committed.status).toBe("committed");
		const reapplied = await applyIntegration(root, journalDir, committed);
		expect(reapplied.status).toBe("committed");
		expect(await loadJournal(journalDir)).toMatchObject({ status: "committed" });
	});

	it("recovers a crash mid-apply by reconciling forward, not duplicating", async () => {
		await makeDirs();
		const journal = await prepareIntegration(root, "base-1", { "src/keep.ts": "fixed" });
		// Simulate a crash after the write landed but before APPLIED persisted.
		await Bun.write(path.join(root, "src/keep.ts"), "fixed");
		await Bun.write(path.join(journalDir, "integration.json"), JSON.stringify({ ...journal, status: "applying" }));
		const recovered = await recoverIntegration(root, journalDir);
		expect(recovered?.status).toBe("applied");
		expect(await Bun.file(path.join(root, "src/keep.ts")).text()).toBe("fixed");
	});

	it("refuses when the tree diverged from the prepared base", async () => {
		await makeDirs();
		const journal = await prepareIntegration(root, "base-1", { "src/keep.ts": "fixed" });
		await Bun.write(path.join(root, "src/keep.ts"), "someone else edited");
		await expect(applyIntegration(root, journalDir, journal)).rejects.toThrow("diverged");
		expect(await loadJournal(journalDir)).toMatchObject({ status: "applying" });
	});

	it("refuses to commit before apply", async () => {
		await makeDirs();
		const journal = await prepareIntegration(root, "base-1", { "src/keep.ts": "fixed" });
		await expect(commitIntegration(journalDir, journal)).rejects.toThrow("cannot commit");
	});

	it("leaves non-applying journals untouched on recovery", async () => {
		await makeDirs();
		expect(await recoverIntegration(root, journalDir)).toBeNull();
	});
});
