/**
 * Durable human decisions (WP-10): bound to workflow+phase+attempt+nonce,
 * consumed exactly once. A stale approval never satisfies a later attempt.
 */

export interface HumanDecision {
	nonce: string;
	workflow: string;
	phase: string;
	attempt: number;
	question: string;
	response: string | null;
	consumed: boolean;
}

function decisionPath(dir: string, nonce: string): string {
	if (!/^[A-Za-z0-9_-]{8,64}$/.test(nonce)) throw new Error("invalid decision nonce");
	return `${dir}/${nonce}.json`;
}

async function persistDecision(dir: string, decision: HumanDecision): Promise<void> {
	await Bun.write(`${dir}/${decision.nonce}.json.tmp`, JSON.stringify(decision));
	const { rename } = await import("node:fs/promises");
	await rename(`${dir}/${decision.nonce}.json.tmp`, decisionPath(dir, decision.nonce));
}

export async function requestDecision(
	dir: string,
	options: { workflow: string; phase: string; attempt: number; question: string },
): Promise<HumanDecision> {
	const nonce = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
	const decision: HumanDecision = { ...options, nonce, response: null, consumed: false };
	await persistDecision(dir, decision);
	return decision;
}

export async function loadDecision(dir: string, nonce: string): Promise<HumanDecision | null> {
	try {
		return (await Bun.file(decisionPath(dir, nonce)).json()) as HumanDecision;
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

/** Resolve exactly once; binding mismatch or replay throws. */
export async function resolveDecision(dir: string, nonce: string, resolution: Resolution): Promise<HumanDecision> {
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
	const resolved: HumanDecision = { ...decision, response: resolution.response, consumed: true };
	await persistDecision(dir, resolved);
	return resolved;
}
