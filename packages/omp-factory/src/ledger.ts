/**
 * Durable workflow ledger (WP-06): `events.jsonl` is authoritative truth,
 * `checkpoint.json` is a reconstructable projection. Deleting the projection
 * never destroys truth; a corrupt log fails loudly, never as "new workflow".
 */
import * as fs from "node:fs/promises";

export interface WorkflowEvent {
	seq: number;
	type: string;
	at: number;
	payload: Record<string, unknown>;
}

export interface PhaseProjection {
	status: "pending" | "ready" | "dispatched" | "accepted" | "failed";
	attempts: number;
	acceptedVersion: number | null;
	integration: "prepared" | "applying" | "applied" | "committed" | null;
}

export interface WorkflowProjection {
	workflowId: string;
	status: "running" | "accepted" | "failed";
	phases: Record<string, PhaseProjection>;
	lastSeq: number;
}

const LOG_FILE = "events.jsonl";
const CHECKPOINT_FILE = "checkpoint.json";
function freshPhase(): PhaseProjection {
	return { status: "pending", attempts: 0, acceptedVersion: null, integration: null };
}

/** Pure reducer: replay must reconstruct the same logical state every time. */
export function reduceEvents(workflowId: string, events: WorkflowEvent[]): WorkflowProjection {
	const projection: WorkflowProjection = { workflowId, status: "running", phases: {}, lastSeq: 0 };
	for (const event of events) {
		if (!Number.isInteger(event.seq) || event.seq !== projection.lastSeq + 1) {
			throw new Error(`ledger gap or reorder at seq ${event.seq}`);
		}
		projection.lastSeq = event.seq;
		const phase = typeof event.payload.phase === "string" ? event.payload.phase : undefined;
		switch (event.type) {
			case "WorkflowStarted":
				break;
			case "PhaseStarted": {
				if (!phase) throw new Error("PhaseStarted without phase");
				projection.phases[phase] ??= freshPhase();
				break;
			}
			case "AttemptStarted": {
				if (!phase) throw new Error("AttemptStarted without phase");
				const current = projection.phases[phase] ?? freshPhase();
				current.status = "dispatched";
				current.attempts += 1;
				projection.phases[phase] = current;
				break;
			}
			// A phase records that it was accepted; the graph records which
			// version that acceptance produced. Splitting them keeps the
			// ordinal out of the hands of the only caller that cannot know it.
			case "PhaseAccepted": {
				if (!phase) throw new Error("PhaseAccepted without phase");
				const current = projection.phases[phase] ?? freshPhase();
				current.status = "accepted";
				projection.phases[phase] = current;
				break;
			}
			case "VersionAccepted": {
				if (!phase) throw new Error("VersionAccepted without phase");
				const version = event.payload.version;
				if (!Number.isInteger(version) || (version as number) < 1) {
					throw new Error("VersionAccepted without a positive integer version");
				}
				const current = projection.phases[phase] ?? freshPhase();
				current.status = "accepted";
				current.acceptedVersion = version as number;
				projection.phases[phase] = current;
				break;
			}
			case "PhaseFailed": {
				if (!phase) throw new Error("PhaseFailed without phase");
				const current = projection.phases[phase] ?? freshPhase();
				current.status = "failed";
				projection.phases[phase] = current;
				break;
			}
			case "IntegrationPrepared":
			case "IntegrationApplying":
			case "IntegrationApplied":
			case "IntegrationCommitted": {
				if (!phase) throw new Error(`${event.type} without phase`);
				const current = projection.phases[phase] ?? freshPhase();
				current.integration =
					event.type === "IntegrationPrepared"
						? "prepared"
						: event.type === "IntegrationApplying"
							? "applying"
							: event.type === "IntegrationApplied"
								? "applied"
								: "committed";
				projection.phases[phase] = current;
				break;
			}
			case "WorkflowAccepted":
				projection.status = "accepted";
				break;
			case "WorkflowFailed":
				projection.status = "failed";
				break;
			default:
				throw new Error(`unknown event type: ${event.type}`);
		}
	}
	return projection;
}

async function readLog(dir: string): Promise<{ workflowId: string; events: WorkflowEvent[] } | null> {
	let text: string;
	try {
		text = await Bun.file(`${dir}/${LOG_FILE}`).text();
	} catch {
		return null;
	}
	const lines = text.split("\n").filter(line => line.trim().length > 0);
	if (lines.length === 0) throw new Error("event log exists but holds no events");
	const events: WorkflowEvent[] = [];
	let first: WorkflowEvent;
	try {
		first = JSON.parse(lines[0] ?? "") as WorkflowEvent;
	} catch {
		throw new Error("corrupt event log line");
	}
	const workflowId = first.payload?.workflowId;
	if (typeof workflowId !== "string" || workflowId.length === 0) {
		throw new Error("event log has no workflow identity");
	}
	for (const line of lines) {
		let event: WorkflowEvent;
		try {
			event = JSON.parse(line) as WorkflowEvent;
		} catch {
			throw new Error("corrupt event log line");
		}
		if (typeof event.seq !== "number" || typeof event.type !== "string" || !event.payload) {
			throw new Error("corrupt event log line");
		}
		events.push(event);
	}
	return { workflowId, events };
}

/**
 * Appends are serialized per run directory and land as real appends.
 * Read-modify-writing the whole log looked fine until concurrent phases
 * shared one ledger: two writers each read the same prefix and the later
 * `write` truncated the other's line, leaving a torn record that replay
 * correctly refused to parse.
 */
const appendChains = new Map<string, Promise<unknown>>();
const nextSeq = new Map<string, number>();

async function appendOnce(
	dir: string,
	workflowId: string,
	type: string,
	payload: Record<string, unknown>,
): Promise<WorkflowEvent> {
	let seq = nextSeq.get(dir);
	if (seq === undefined) {
		const existing = await readLog(dir);
		seq = existing ? existing.events.length : 0;
	}
	seq += 1;
	const event: WorkflowEvent = { seq, type, at: Date.now(), payload: { workflowId, ...payload } };
	await fs.appendFile(`${dir}/${LOG_FILE}`, `${JSON.stringify(event)}\n`);
	nextSeq.set(dir, seq);
	return event;
}

export async function appendEvent(
	dir: string,
	workflowId: string,
	type: string,
	payload: Record<string, unknown>,
): Promise<WorkflowEvent> {
	const previous = appendChains.get(dir) ?? Promise.resolve();
	const next = previous.then(
		() => appendOnce(dir, workflowId, type, payload),
		() => appendOnce(dir, workflowId, type, payload),
	);
	appendChains.set(
		dir,
		next.catch(() => undefined),
	);
	return next;
}

/** Atomic checkpoint write (tmp + rename): a torn write never parses as truth. */
export async function writeCheckpoint(dir: string, projection: WorkflowProjection): Promise<void> {
	await Bun.write(`${dir}/${CHECKPOINT_FILE}.tmp`, JSON.stringify(projection));
	const { rename } = await import("node:fs/promises");
	await rename(`${dir}/${CHECKPOINT_FILE}.tmp`, `${dir}/${CHECKPOINT_FILE}`);
}

export async function loadCheckpoint(dir: string): Promise<WorkflowProjection | null> {
	try {
		return (await Bun.file(`${dir}/${CHECKPOINT_FILE}`).json()) as WorkflowProjection;
	} catch {
		return null;
	}
}

export interface ReplayResult {
	projection: WorkflowProjection;
	checkpointUsed: boolean;
}

/**
 * Rebuild state: a valid checkpoint matching the log wins; otherwise replay
 * the authoritative log. A corrupt log throws — never silently "new workflow".
 */
export async function replay(dir: string): Promise<ReplayResult> {
	const logged = await readLog(dir);
	if (!logged) {
		throw new Error("no event log: refusing to invent workflow state");
	}
	const checkpoint = await loadCheckpoint(dir);
	if (checkpoint && checkpoint.workflowId === logged.workflowId && checkpoint.lastSeq === logged.events.length) {
		return { projection: checkpoint, checkpointUsed: true };
	}
	return { projection: reduceEvents(logged.workflowId, logged.events), checkpointUsed: false };
}
