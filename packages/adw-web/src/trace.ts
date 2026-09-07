/**
 * Server-side decoding of a run's binary trace.
 *
 * The native bindings cannot load in a browser, so this is the only place that
 * touches `events.bin` — the page receives plain JSON. Decoding stays behind
 * `TaskTraceReader` rather than re-implementing the record layout here, so
 * there is exactly one reader of the format per process.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { TaskTraceReader, taskTraceLayout } from "@oh-my-pi/pi-natives";

/** One decoded event, with its interned ids already resolved. */
export interface TraceEvent {
	seq: number;
	ts: number;
	kind: string;
	ok: boolean;
	attempt: number;
	phase: string;
	owner: string;
	gate: string;
	detail: string;
	value: number;
}

export interface RunSummary {
	adwId: string;
	workflow: string;
	/** Wall-clock ms from the first event to the last. */
	durationMs: number;
	startedAt: number;
	/** `null` while the run is still going — no terminal record yet. */
	accepted: boolean | null;
	phases: number;
	events: number;
	resumed: boolean;
}

export interface RunDetail extends RunSummary {
	events: number;
	timeline: TraceEvent[];
}

/** Runs live outside any session dir so a dead run stays findable. */
export function runsRoot(): string {
	return path.join(process.env.ADW_RUNS_DIR ?? path.join(process.env.HOME ?? ".", ".omp/agent/adw"));
}

function decode(traceDir: string): TraceEvent[] {
	const reader = new TaskTraceReader(traceDir);
	const raw = reader.readRaw(0);
	const strings = reader.strings();
	const layout = TRACE_LAYOUT;
	const view = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
	const text = (id: number) => (id === 0 ? "" : (strings[id - 1] ?? ""));

	const events: TraceEvent[] = [];
	for (let seq = 0; seq * layout.recordLen < raw.length; seq++) {
		const at = seq * layout.recordLen;
		events.push({
			seq,
			ts: Number(view.getBigUint64(at + layout.offTs, true)),
			kind: layout.kindNames[view.getUint8(at + layout.offKind)] ?? "unknown",
			ok: (view.getUint8(at + layout.offFlags) & layout.flagOk) !== 0,
			attempt: view.getUint16(at + layout.offAttempt, true),
			phase: text(view.getUint32(at + layout.offPhase, true)),
			owner: text(view.getUint32(at + layout.offOwner, true)),
			gate: text(view.getUint32(at + layout.offGate, true)),
			detail: text(view.getUint32(at + layout.offDetail, true)),
			value: view.getUint32(at + layout.offValue, true),
		});
	}
	return events;
}

// Read once: the layout is a compile-time constant of the engine, not per-run
// state, and re-crossing the boundary per request buys nothing.
const TRACE_LAYOUT = taskTraceLayout();

function summarize(adwId: string, events: TraceEvent[]): RunSummary {
	const finished = events.find(event => event.kind === "run_finished");
	const started = events.find(event => event.kind === "run_started");
	const first = events[0]?.ts ?? 0;
	return {
		adwId,
		workflow: started?.detail || "unknown",
		startedAt: first,
		durationMs: (events.at(-1)?.ts ?? first) - first,
		// A run with no terminal record is either still going or was killed; both
		// are honestly "not settled", never "failed".
		accepted: finished ? finished.ok : null,
		phases: events.filter(event => event.kind === "phase_finished").length,
		events: events.length,
		resumed: events.some(event => event.kind === "run_resumed"),
	};
}

export function listRuns(): RunSummary[] {
	const root = runsRoot();
	if (!fs.existsSync(root)) return [];
	const runs: RunSummary[] = [];
	for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
		if (!entry.isDirectory()) continue;
		const traceDir = path.join(root, entry.name, "trace");
		if (!fs.existsSync(path.join(traceDir, "events.bin"))) continue;
		try {
			runs.push(summarize(entry.name, decode(traceDir)));
		} catch {
			// A half-written or foreign trace is skipped rather than failing the
			// whole listing; the reader already rejects a bad header.
		}
	}
	return runs.sort((a, b) => b.startedAt - a.startedAt);
}

export function readRun(adwId: string): RunDetail | null {
	const traceDir = path.join(runsRoot(), adwId, "trace");
	if (!fs.existsSync(path.join(traceDir, "events.bin"))) return null;
	const timeline = decode(traceDir);
	return { ...summarize(adwId, timeline), timeline };
}
