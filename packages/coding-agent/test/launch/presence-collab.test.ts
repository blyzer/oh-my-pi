/**
 * Discovery contract for attaching to a live session: a process publishes the
 * socket its local collab room listens on, a peer finds it by reading the
 * project scope, and everything that could make that record a lie — an older
 * omp, a dead process, a retracted room, a torn write — resolves to "not a
 * candidate" rather than to a bad connect.
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, describe, expect, it } from "bun:test";
import {
	type DaemonPresenceRecord,
	findLiveCollabSessions,
	registerDaemonProjectPresence,
} from "@oh-my-pi/pi-coding-agent/launch/presence";

const roots: string[] = [];

async function makeRuntimeDir(): Promise<string> {
	const root = await fs.mkdtemp(path.join(os.tmpdir(), "presence-collab-"));
	roots.push(root);
	return root;
}

async function writeRawRecord(runtimeDir: string, name: string, record: unknown): Promise<void> {
	const clientsDir = path.join(runtimeDir, "clients");
	await fs.mkdir(clientsDir, { recursive: true, mode: 0o700 });
	await Bun.write(path.join(clientsDir, `${name}.json`), JSON.stringify(record));
}

describe("live collab session discovery", () => {
	afterEach(async () => {
		for (const root of roots.splice(0)) await fs.rm(root, { recursive: true, force: true });
	});

	it("finds a room the owning process published, and drops it when the room stops", async () => {
		const runtimeDir = await makeRuntimeDir();
		const presence = await registerDaemonProjectPresence("/tmp/project", runtimeDir);

		// A registered process with no room is not a candidate: the field a peer
		// connects on is absent, which is the same state an older omp leaves.
		expect(await findLiveCollabSessions(runtimeDir)).toEqual([]);

		expect(
			await presence.update({
				collabSocket: "/tmp/collab.sock",
				sessionId: "sess-42",
				sessionFile: "/transcripts/sess-42.jsonl",
			}),
		).toBe(true);

		const found = await findLiveCollabSessions(runtimeDir);
		expect(found.length).toBe(1);
		expect(found[0]?.collabSocket).toBe("/tmp/collab.sock");
		expect(found[0]?.sessionId).toBe("sess-42");
		expect(found[0]?.sessionFile).toBe("/transcripts/sess-42.jsonl");
		expect(found[0]?.pid).toBe(process.pid);

		// Retraction is what teardown does, and it must make the process
		// undiscoverable again without removing its presence entry.
		await presence.update({ collabSocket: undefined });
		expect(await findLiveCollabSessions(runtimeDir)).toEqual([]);

		await presence.close();
	});

	it("bumps generation on every update so a peer can tell rebinds apart", async () => {
		const runtimeDir = await makeRuntimeDir();
		const presence = await registerDaemonProjectPresence("/tmp/project", runtimeDir);

		await presence.update({ collabSocket: "/tmp/a.sock", sessionId: "sess-a" });
		const first = (await findLiveCollabSessions(runtimeDir))[0];
		await presence.update({ collabSocket: "/tmp/a.sock", sessionId: "sess-b" });
		const second = (await findLiveCollabSessions(runtimeDir))[0];

		expect(second?.generation).toBeGreaterThan(first?.generation ?? 0);
		expect(second?.sessionId).toBe("sess-b");
		await presence.close();
	});

	it("ignores a record whose process is gone", async () => {
		const runtimeDir = await makeRuntimeDir();
		// pid 1 is init and always alive, so a dead pid has to be one this
		// machine cannot be running: a freshly exited child is the honest way.
		const proc = Bun.spawn(["true"]);
		await proc.exited;
		await writeRawRecord(runtimeDir, "dead", {
			pid: proc.pid,
			id: `${proc.pid}-dead`,
			projectDir: "/tmp/project",
			collabSocket: "/tmp/dead.sock",
		} satisfies DaemonPresenceRecord);

		expect(await findLiveCollabSessions(runtimeDir)).toEqual([]);
	});

	it("ignores an older three-field record instead of failing on it", async () => {
		const runtimeDir = await makeRuntimeDir();
		await writeRawRecord(runtimeDir, "legacy", {
			pid: process.pid,
			id: `${process.pid}-legacy`,
			projectDir: "/tmp/project",
		});

		// The point is that it does not throw and does not match: an 18.1.x omp
		// keeps writing exactly this shape, and it is simply not attachable.
		expect(await findLiveCollabSessions(runtimeDir)).toEqual([]);
	});

	it("survives a malformed record without discarding its live neighbours", async () => {
		const runtimeDir = await makeRuntimeDir();
		const clientsDir = path.join(runtimeDir, "clients");
		await fs.mkdir(clientsDir, { recursive: true, mode: 0o700 });
		await Bun.write(path.join(clientsDir, "torn.json"), '{"pid": 42, "collabSo');
		await writeRawRecord(runtimeDir, "good", {
			pid: process.pid,
			id: `${process.pid}-good`,
			projectDir: "/tmp/project",
			collabSocket: "/tmp/good.sock",
		} satisfies DaemonPresenceRecord);

		const found = await findLiveCollabSessions(runtimeDir);
		expect(found.length).toBe(1);
		expect(found[0]?.collabSocket).toBe("/tmp/good.sock");
		// Discovery is read-only: the unreadable entry is skipped, never swept.
		// Pruning belongs to hasLiveDaemonProjectPresence, and a reader that
		// deletes can delete a record another process is mid-rewrite on.
		expect(await Bun.file(path.join(clientsDir, "torn.json")).exists()).toBe(true);
	});

	it("returns nothing for a scope that was never registered", async () => {
		const runtimeDir = await makeRuntimeDir();
		expect(await findLiveCollabSessions(runtimeDir)).toEqual([]);
	});

	it("refuses to update after close", async () => {
		const runtimeDir = await makeRuntimeDir();
		const presence = await registerDaemonProjectPresence("/tmp/project", runtimeDir);
		await presence.close();
		expect(await presence.update({ collabSocket: "/tmp/late.sock" })).toBe(false);
		expect(await findLiveCollabSessions(runtimeDir)).toEqual([]);
	});
});
