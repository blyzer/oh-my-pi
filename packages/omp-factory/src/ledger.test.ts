import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { appendEvent, loadCheckpoint, replay, writeCheckpoint } from "./ledger";

let dir = "";

async function makeDir(): Promise<string> {
	dir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-ledger-"));
	return dir;
}

afterEach(async () => {
	if (dir) await fs.rm(dir, { recursive: true, force: true });
	dir = "";
});

async function driveWorkflow(root: string): Promise<void> {
	await appendEvent(root, "wf-1", "WorkflowStarted", {});
	await appendEvent(root, "wf-1", "AttemptStarted", { phase: "build" });
	await appendEvent(root, "wf-1", "VersionAccepted", { phase: "build", version: 1 });
	await appendEvent(root, "wf-1", "WorkflowAccepted", {});
}

describe("ledger replay", () => {
	it("reconstructs accepted state from the event log", async () => {
		const root = await makeDir();
		await driveWorkflow(root);
		const { projection, checkpointUsed } = await replay(root);
		expect(checkpointUsed).toBeFalse();
		expect(projection.status).toBe("accepted");
		expect(projection.phases.build).toMatchObject({ status: "accepted", attempts: 1, acceptedVersion: 1 });
		expect(projection.lastSeq).toBe(4);
	});

	it("survives checkpoint loss without changing logical state", async () => {
		const root = await makeDir();
		await driveWorkflow(root);
		const before = await replay(root);
		await writeCheckpoint(root, before.projection);
		await fs.rm(path.join(root, "checkpoint.json"));
		expect(await loadCheckpoint(root)).toBeNull();
		const after = await replay(root);
		expect(after.projection).toEqual(before.projection);
	});

	it("uses a matching checkpoint instead of replaying", async () => {
		const root = await makeDir();
		await driveWorkflow(root);
		const first = await replay(root);
		await writeCheckpoint(root, first.projection);
		const second = await replay(root);
		expect(second.checkpointUsed).toBeTrue();
		expect(second.projection).toEqual(first.projection);
	});

	it("refuses to invent state when no log exists", async () => {
		const root = await makeDir();
		await expect(replay(root)).rejects.toThrow("no event log");
	});

	it("fails loudly on a corrupt log line instead of truncating truth", async () => {
		const root = await makeDir();
		await appendEvent(root, "wf-1", "WorkflowStarted", {});
		await Bun.write(path.join(root, "events.jsonl"), "NOT-JSON{{{");
		await expect(replay(root)).rejects.toThrow("corrupt");
	});

	it("rejects unknown event types on replay", async () => {
		const root = await makeDir();
		await appendEvent(root, "wf-1", "WorkflowStarted", {});
		await appendEvent(root, "wf-1", "TimeTravel", {});
		await expect(replay(root)).rejects.toThrow("unknown event type");
	});

	// Regression: appends used to read the whole log and rewrite it, so two
	// concurrent phases sharing a run directory truncated each other's line
	// and replay refused the torn record.
	it("keeps a gapless log under concurrent appends", async () => {
		const root = await makeDir();
		await Promise.all(
			Array.from({ length: 24 }, (_unused, index) =>
				appendEvent(root, "wf-1", "AttemptStarted", { phase: `p${index}`, attempt: 1 }),
			),
		);
		const text = await Bun.file(path.join(root, "events.jsonl")).text();
		const lines = text.split("\n").filter(line => line.trim().length > 0);
		expect(lines).toHaveLength(24);
		const seqs = lines.map(line => (JSON.parse(line) as { seq: number }).seq).sort((a, b) => a - b);
		expect(seqs).toEqual(Array.from({ length: 24 }, (_unused, index) => index + 1));
	});
});
