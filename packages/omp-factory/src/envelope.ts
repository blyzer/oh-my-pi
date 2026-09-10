/**
 * Builder envelope: the last complete top-level JSON object in the builder's
 * final message is its only structured channel. Prose, fences and reasoning
 * before it are ignored; a missing or malformed object is a contract
 * violation, never a silent success.
 */

export interface Envelope {
	status: "success" | "fail";
	summary: string;
	artifacts: string[];
	notesForNextAgent?: string;
}

export type EnvelopeResult = { ok: true; envelope: Envelope } | { ok: false; violation: string };

/**
 * Span of the last balanced top-level `{...}` in `text`, or null.
 * String literals (and their escapes) never split a span, so a brace inside
 * `"summary"` cannot truncate the object; an unterminated object is not a
 * candidate at all.
 */
function lastObjectSpan(text: string): string | null {
	let depth = 0;
	let start = -1;
	let inString = false;
	let escaped = false;
	let last: string | null = null;
	for (let i = 0; i < text.length; i++) {
		const ch = text[i];
		if (inString) {
			if (escaped) escaped = false;
			else if (ch === "\\") escaped = true;
			else if (ch === '"') inString = false;
			continue;
		}
		if (ch === '"') {
			inString = true;
			continue;
		}
		if (ch === "{") {
			if (depth === 0) start = i;
			depth += 1;
			continue;
		}
		if (ch === "}") {
			if (depth === 0) continue;
			depth -= 1;
			if (depth === 0 && start >= 0) last = text.slice(start, i + 1);
		}
	}
	return last;
}

function stringArray(value: unknown): string[] | null {
	if (!Array.isArray(value)) return null;
	const out: string[] = [];
	for (const entry of value) {
		if (typeof entry !== "string") return null;
		out.push(entry);
	}
	return out;
}

/** Parse a builder turn. Every rejection names what the contract required. */
export function parseEnvelope(turn: string): EnvelopeResult {
	const span = lastObjectSpan(turn);
	if (span === null) return { ok: false, violation: "no top-level JSON object in the final message" };
	let parsed: unknown;
	try {
		parsed = JSON.parse(span);
	} catch (error) {
		return { ok: false, violation: `envelope is not valid JSON: ${(error as Error).message}` };
	}
	if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
		return { ok: false, violation: "envelope must be a JSON object" };
	}
	const record = parsed as Record<string, unknown>;
	const status = record.status;
	if (status !== "success" && status !== "fail") {
		return { ok: false, violation: `unknown status: ${JSON.stringify(status ?? null)}` };
	}
	const summary = record.summary;
	if (typeof summary !== "string" || summary.trim().length === 0) {
		return { ok: false, violation: "envelope requires a non-empty summary" };
	}
	const artifacts = record.artifacts === undefined ? [] : stringArray(record.artifacts);
	if (artifacts === null) return { ok: false, violation: "artifacts must be an array of strings" };
	const notes = record.notes_for_next_agent;
	return {
		ok: true,
		envelope: {
			status,
			summary,
			artifacts,
			notesForNextAgent: typeof notes === "string" ? notes : undefined,
		},
	};
}
