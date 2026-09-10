/**
 * Durable workflow ledger (WP-06): `events.jsonl` is authoritative truth,
 * `checkpoint.json` is a reconstructable projection. Deleting the projection
 * never destroys truth; a corrupt log fails loudly, never as "new workflow".
 */

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
	return { status: "pending", attempts: 0, acceptedVersion: null };
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
			case "AttemptStarted": {
				if (!phase) throw new Error("AttemptStarted without phase");
				const current = projection.phases[phase] ?? freshPhase();
				current.status = "dispatched";
				current.attempts += 1;
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

export async function appendEvent(
	dir: string,
	workflowId: string,
	type: string,
	payload: Record<string, unknown>,
): Promise<WorkflowEvent> {
	const existing = await readLog(dir);
	const seq = existing ? existing.events.length + 1 : 1;
	const event: WorkflowEvent = { seq, type, at: Date.now(), payload: { workflowId, ...payload } };
	const file = Bun.file(`${dir}/${LOG_FILE}`);
	const handle = (await file.exists()) ? await file.text() : "";
	await Bun.write(`${dir}/${LOG_FILE}`, `${handle}${JSON.stringify(event)}\n`);
	return event;
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
