import type { Dirent } from "node:fs";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import { isEnoent, logger, postmortem } from "@oh-my-pi/pi-utils";
import { canonicalProjectDir, daemonRuntimeDir } from "./paths";

const CLIENTS_DIR = "clients";
const BROKER_PID_FILE = "broker.pid";
/**
 * Basename of the container holding per-project daemon scopes
 * (`<state>/run/daemons`). {@link pruneDeadDaemonRuntimeDirs} refuses to sweep
 * any other root so a runtime dir passed from outside the state tree cannot
 * turn the reclaim into an rm -rf of unrelated neighbours (issue #8721).
 */
const DAEMONS_DIR = "daemons";
/**
 * Name shape of a project daemon scope: the 16-hex wyhash of the project dir
 * produced by `getDaemonRuntimeDir`. Only entries matching this are pruned,
 * which excludes the machine-global `global` container and any foreign dir.
 */
const DAEMON_SCOPE_KEY = /^[0-9a-f]{16}$/;
/**
 * Grace before a dead daemon runtime dir becomes prune-eligible. Guards against
 * deleting a scope whose owning omp process is mid-startup (token written, broker
 * not yet spawned, presence not yet registered). The leak this reclaims is a
 * weeks-scale accumulation, so a few minutes of slack costs nothing.
 */
const DAEMON_RUNTIME_STALE_GRACE_MS = 5 * 60_000;

/**
 * What one omp process publishes about itself in a project daemon scope.
 *
 * `pid`, `id` and `projectDir` have always been written and are required. The
 * rest are OPTIONAL on purpose: an older omp writes a three-field record, and a
 * reader that treats the newer fields as mandatory would decide those sessions
 * do not exist. Absent means "this process does not offer that", never "this
 * record is malformed".
 */
export interface DaemonPresenceRecord {
	pid: number;
	id: string;
	projectDir: string;
	/** Session the process currently has open. Changes on `/resume` and branch. */
	sessionId?: string;
	/** Absolute transcript path, which is what a peer matches a session on. */
	sessionFile?: string;
	/**
	 * Unix socket a local collab room is listening on, when one is running.
	 * Presence is the only place this is discoverable — `CollabHost` otherwise
	 * holds its endpoint purely in memory.
	 */
	collabSocket?: string;
	/**
	 * Bumped every time the process rebinds identity (session switch, room
	 * restart). A peer that cached `(pid, generation)` can tell "same process,
	 * different session" from "same session, still mine" without racing.
	 */
	generation?: number;
	/** ISO timestamp of registration, for ordering and staleness display. */
	startedAt?: string;
}

/** Fields a live process may rewrite about itself after registering. */
export type DaemonPresenceUpdate = Partial<
	Pick<DaemonPresenceRecord, "sessionId" | "sessionFile" | "collabSocket">
>;

/** Handle keeping one omp process registered in a project daemon scope. */
export interface DaemonProjectPresence {
	/**
	 * Rewrite this process's own record. Bumps `generation` on every call.
	 *
	 * Failure is reported, never thrown: presence is an optimisation for peers,
	 * and a session must not die because a directory entry could not be
	 * refreshed. Returns whether the write landed, so a caller that cares can
	 * retry — and it SHOULD retry rather than latch off, because the failures
	 * that reach here (EACCES/EMFILE while the runtime dir is recreated, a
	 * concurrent sweep) are transient, and giving up permanently drops a live
	 * session out of the directory for the rest of the process lifetime.
	 */
	update(fields: DaemonPresenceUpdate): Promise<boolean>;
	close(): Promise<void>;
}

/** Register this omp process so project daemons survive while it remains alive. */
export async function registerDaemonProjectPresence(
	projectDir: string,
	runtimeOverride?: string,
): Promise<DaemonProjectPresence> {
	const canonical = await canonicalProjectDir(projectDir);
	const runtimeDir = runtimeOverride ?? daemonRuntimeDir(canonical);
	const clientsDir = path.join(runtimeDir, CLIENTS_DIR);
	await fs.mkdir(clientsDir, { recursive: true, mode: 0o700 });
	const id = `${process.pid}-${crypto.randomUUID()}`;
	const presencePath = path.join(clientsDir, `${id}.json`);
	let record: DaemonPresenceRecord = {
		pid: process.pid,
		id,
		projectDir: canonical,
		generation: 0,
		startedAt: new Date().toISOString(),
	};
	await writePresenceRecord(presencePath, record);
	let closed = false;
	const update = async (fields: DaemonPresenceUpdate): Promise<boolean> => {
		if (closed) return false;
		const next: DaemonPresenceRecord = { ...record, ...fields, generation: (record.generation ?? 0) + 1 };
		try {
			await writePresenceRecord(presencePath, next);
		} catch (error) {
			// Transient by assumption — see the doc comment on the interface.
			logger.debug("presence: update failed", { id, error: String(error) });
			return false;
		}
		record = next;
		return true;
	};
	const close = async (): Promise<void> => {
		if (closed) return;
		closed = true;
		cancelCleanup();
		await fs.rm(presencePath, { force: true });
	};
	const cancelCleanup = postmortem.register(`daemon-presence:${id}`, () => close());
	return { update, close };
}

/**
 * Write the record so a concurrent reader sees the old bytes or the new ones,
 * never a prefix. A peer polls this directory, and the original single `write`
 * was safe only because the record never changed after registration; now that
 * it does, a torn read would look like a malformed record and get the entry
 * swept by {@link hasLiveDaemonProjectPresence}.
 */
async function writePresenceRecord(presencePath: string, record: DaemonPresenceRecord): Promise<void> {
	const tempPath = `${presencePath}.${process.pid}.tmp`;
	await Bun.write(tempPath, JSON.stringify(record));
	await fs.chmod(tempPath, 0o600);
	await fs.rename(tempPath, presencePath);
}

/** Return whether a registered omp process in this runtime directory is still alive. */
export async function hasLiveDaemonProjectPresence(runtimeDir: string): Promise<boolean> {
	const clientsDir = path.join(runtimeDir, CLIENTS_DIR);
	let entries: string[];
	try {
		entries = await fs.readdir(clientsDir);
	} catch (error) {
		if (isEnoent(error)) return false;
		throw error;
	}
	let live = false;
	for (const entry of entries) {
		const presencePath = path.join(clientsDir, entry);
		try {
			const decoded: unknown = await Bun.file(presencePath).json();
			if (
				typeof decoded !== "object" ||
				decoded === null ||
				!("pid" in decoded) ||
				typeof decoded.pid !== "number"
			) {
				await fs.rm(presencePath, { force: true });
				continue;
			}
			try {
				process.kill(decoded.pid, 0);
				live = true;
			} catch {
				await fs.rm(presencePath, { force: true });
			}
		} catch (error) {
			if (!isEnoent(error)) await fs.rm(presencePath, { force: true });
		}
	}
	return live;
}

/**
 * Live omp processes in a project scope that are offering a local collab room.
 *
 * This is the discovery half of the attach story: `CollabHost` holds its
 * endpoint in memory, so without a published record a sibling process has no
 * way to learn that a live session is reachable. Every field the caller filters
 * on is optional in the record, so an older omp — or a newer one with no room
 * open — is simply not a candidate rather than an error.
 *
 * Read-only by design. Unlike {@link hasLiveDaemonProjectPresence} this never
 * deletes anything: a reader that prunes is a reader that can delete a record
 * some other process is mid-rewrite on, and pruning already has an owner.
 */
export async function findLiveCollabSessions(runtimeDir: string): Promise<DaemonPresenceRecord[]> {
	const clientsDir = path.join(runtimeDir, CLIENTS_DIR);
	let entries: string[];
	try {
		entries = await fs.readdir(clientsDir);
	} catch (error) {
		if (isEnoent(error)) return [];
		throw error;
	}
	const found: DaemonPresenceRecord[] = [];
	for (const entry of entries) {
		let decoded: unknown;
		try {
			decoded = await Bun.file(path.join(clientsDir, entry)).json();
		} catch {
			continue;
		}
		if (typeof decoded !== "object" || decoded === null) continue;
		const record = decoded as DaemonPresenceRecord;
		if (typeof record.pid !== "number" || typeof record.collabSocket !== "string") continue;
		// A record outlives a crash, so the pid probe is what separates an
		// attachable session from a leftover. Cheap, and the only liveness
		// signal that does not require connecting.
		try {
			process.kill(record.pid, 0);
		} catch {
			continue;
		}
		found.push(record);
	}
	return found;
}

/** PID recorded in the runtime dir's broker lease when that broker process is still alive; undefined otherwise. */
export async function readLiveDaemonBrokerPid(runtimeDir: string): Promise<number | undefined> {
	let raw: unknown;
	try {
		raw = await Bun.file(path.join(runtimeDir, BROKER_PID_FILE)).json();
	} catch {
		return undefined; // Missing or malformed broker.pid => no owning broker.
	}
	if (typeof raw !== "object" || raw === null || !("pid" in raw) || typeof raw.pid !== "number") {
		return undefined;
	}
	try {
		process.kill(raw.pid, 0);
		return raw.pid;
	} catch {
		return undefined;
	}
}

/**
 * Remove sibling project daemon runtime directories whose broker is dead and
 * whose client-presence set is empty, reclaiming the disk that short-lived
 * project directories leave behind (issue #8674).
 *
 * Best-effort and non-throwing: a scope is deleted only when its `broker.pid`
 * is absent/dead, no live client presence remains, and it has been untouched
 * for {@link DAEMON_RUNTIME_STALE_GRACE_MS}. The caller's own `currentRuntimeDir`
 * is always skipped, and the sweep runs only inside the {@link DAEMONS_DIR}
 * container over entries named like a {@link DAEMON_SCOPE_KEY} — so a runtime
 * dir relocated elsewhere (e.g. the smoke test under `os.tmpdir()`) never
 * reclaims unrelated neighbours (issue #8721).
 */
export async function pruneDeadDaemonRuntimeDirs(currentRuntimeDir: string): Promise<void> {
	const root = path.dirname(currentRuntimeDir);
	if (path.basename(root) !== DAEMONS_DIR) return;
	const current = path.resolve(currentRuntimeDir);
	let entries: Dirent[];
	try {
		entries = await fs.readdir(root, { withFileTypes: true });
	} catch (error) {
		if (!isEnoent(error)) {
			logger.warn("Failed to scan daemon runtime root for pruning", {
				root,
				error: error instanceof Error ? error.message : String(error),
			});
		}
		return;
	}
	const now = Date.now();
	for (const entry of entries) {
		if (!entry.isDirectory() || !DAEMON_SCOPE_KEY.test(entry.name)) continue;
		const dir = path.join(root, entry.name);
		if (path.resolve(dir) === current) continue;
		try {
			const stat = await fs.stat(dir);
			if (now - stat.mtimeMs < DAEMON_RUNTIME_STALE_GRACE_MS) continue;
			if ((await readLiveDaemonBrokerPid(dir)) !== undefined) continue;
			if (await hasLiveDaemonProjectPresence(dir)) continue;
			await fs.rm(dir, { recursive: true, force: true });
		} catch (error) {
			if (isEnoent(error)) continue;
			logger.warn("Failed to prune dead daemon runtime dir", {
				dir,
				error: error instanceof Error ? error.message : String(error),
			});
		}
	}
}
