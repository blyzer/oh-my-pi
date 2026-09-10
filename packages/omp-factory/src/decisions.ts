/**
 * Durable human decisions: bound to workflow+phase+attempt+nonce, consumed
 * exactly once. A stale approval never satisfies a later attempt.
 *
 * The nonce is the caller's, never the file's. Deriving a path from the
 * value read out of a decision body lets that body choose where the next
 * write lands — a decision at `a.json` claiming `nonce: "b"` would resolve
 * by writing `b.json`, leaving `a.json` unconsumed and replayable, and a
 * body carrying `../../x` would escape the directory entirely.
 */
import { open, rename, unlink } from "node:fs/promises";

export interface HumanDecision {
	nonce: string;
	workflow: string;
	phase: string;
	attempt: number;
	question: string;
	response: string | null;
	consumed: boolean;
}

const NONCE = /^[A-Za-z0-9_-]{8,64}$/;

function decisionPath(dir: string, nonce: string): string {
	if (!NONCE.test(nonce)) throw new Error("invalid decision nonce");
	return `${dir}/${nonce}.json`;
}

/**
 * Write under the CALLER's nonce, validated before any path is touched.
 * The temporary file is derived from the same validated value, so a hostile
 * body cannot direct the write that precedes validation.
 */
async function persistDecision(dir: string, nonce: string, decision: HumanDecision): Promise<void> {
	const target = decisionPath(dir, nonce);
	const tmp = `${target}.tmp`;
	await Bun.write(tmp, JSON.stringify({ ...decision, nonce }));
	await rename(tmp, target);
}

export async function requestDecision(
	dir: string,
	options: { workflow: string; phase: string; attempt: number; question: string },
): Promise<HumanDecision> {
	const nonce = `${Date.now().toString(36)}-${Bun.randomUUIDv7().replace(/-/g, "").slice(0, 12)}`;
	const decision: HumanDecision = { ...options, nonce, response: null, consumed: false };
	await persistDecision(dir, nonce, decision);
	return decision;
}

export async function loadDecision(dir: string, nonce: string): Promise<HumanDecision | null> {
	// Validate outside the catch: an unreadable file means "no decision", but
	// a malformed nonce is a caller error, and collapsing the two would
	// report an attempted escape as an ordinary miss.
	const target = decisionPath(dir, nonce);
	try {
		return (await Bun.file(target).json()) as HumanDecision;
	} catch {
		return null;
	}
}

export interface Resolution {
	workflow: string;
	phase: string;
	attempt: number;
	response: string;
}

/**
 * Resolve exactly once; binding mismatch or replay throws.
 *
 * The claim is an exclusive create (`wx`) of a marker file, not a
 * read-then-write: load-check-write spans two awaits with nothing holding
 * the decision in between, and the file backing means the racing resolvers
 * can be separate processes. Whoever creates the marker owns the
 * resolution; everyone else sees the decision as already consumed.
 */
export async function resolveDecision(dir: string, nonce: string, resolution: Resolution): Promise<HumanDecision> {
	const target = decisionPath(dir, nonce);
	const decision = await loadDecision(dir, nonce);
	if (!decision) throw new Error("unknown decision nonce");
	if (decision.consumed) throw new Error("decision already consumed");
	if (
		decision.workflow !== resolution.workflow ||
		decision.phase !== resolution.phase ||
		decision.attempt !== resolution.attempt
	) {
		throw new Error("stale decision: binding does not match this attempt");
	}

	let claim: Awaited<ReturnType<typeof open>>;
	try {
		claim = await open(`${target}.claim`, "wx");
	} catch {
		throw new Error("decision already consumed");
	}
	try {
		const resolved: HumanDecision = { ...decision, nonce, response: resolution.response, consumed: true };
		await persistDecision(dir, nonce, resolved);
		return resolved;
	} catch (error) {
		// The claim outlives only a successful resolution: a write that never
		// landed must not permanently block the decision it failed to record.
		await claim.close();
		await unlink(`${target}.claim`).catch(() => undefined);
		throw error;
	} finally {
		await claim.close().catch(() => undefined);
	}
}
