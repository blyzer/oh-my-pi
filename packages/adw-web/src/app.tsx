import { useEffect, useState } from "react";
import type { RunDetail, RunSummary, TraceEvent } from "./trace";

/** Events grouped into the phase they belong to, in trace order. */
interface PhaseGroup {
	name: string;
	owner: string;
	kind: "agent" | "code";
	attempts: number;
	/** Summed across attempts, so a phase that needed three tries shows all three. */
	tokens: number;
	passed: boolean | null;
	startedAt: number;
	endedAt: number;
	events: TraceEvent[];
}

/**
 * The trace is a flat event stream; a phase is whatever happened between its
 * `phase_started` and its `phase_finished`. Grouping here rather than in the
 * engine keeps the record format numeric and lets a reader decide what a
 * "phase" means for display.
 */
function groupPhases(timeline: TraceEvent[]): PhaseGroup[] {
	const groups: PhaseGroup[] = [];
	for (const event of timeline) {
		if (!event.phase) continue;
		let group = groups.at(-1);
		if (!group || group.name !== event.phase || group.passed !== null) {
			group = {
				name: event.phase,
				owner: event.owner,
				// Only `code` phases run a command; everything else spends tokens.
				kind: "agent",
				attempts: 0,
				tokens: 0,
				passed: null,
				startedAt: event.ts,
				endedAt: event.ts,
				events: [],
			};
			groups.push(group);
		}
		if (event.owner) group.owner = event.owner;
		group.attempts = Math.max(group.attempts, event.attempt);
		group.endedAt = event.ts;
		if (event.kind === "phase_tokens" || event.kind === "panel_opinion") group.tokens += event.value;
		group.events.push(event);
		if (event.kind === "phase_finished") group.passed = event.ok;
	}
	return groups;
}

function describe(event: TraceEvent): string {
	switch (event.kind) {
		case "gate_check":
			return `${event.gate}: ${event.detail}`;
		case "panel_opinion":
			return `${event.owner} — ${event.value.toLocaleString()} tokens`;
		case "phase_tokens":
			// A model turn cannot cost nothing; a zero is a provider that
			// reported no usage, and saying "0 tokens" would read as free.
			return event.ok
				? `${event.owner} — ${event.value.toLocaleString()} tokens${event.attempt > 1 ? ` (attempt ${event.attempt})` : ""}`
				: `${event.owner} — provider reported no usage`;
		case "phase_rejected":
			return `${event.value} violation${event.value === 1 ? "" : "s"}${event.detail ? ` · ${event.detail}` : ""}`;
		default:
			return event.detail;
	}
}

function toneOf(event: TraceEvent): string {
	if (event.kind === "gate_check" || event.kind === "phase_finished") return event.ok ? "ok" : "bad";
	if (event.kind === "phase_rejected" || event.kind === "phase_retry") return "warn";
	return "";
}

function Phase({ group, widest }: { group: PhaseGroup; widest: number }) {
	const ms = Math.max(group.endedAt - group.startedAt, 0);
	const mark = group.passed === null ? "◌" : group.passed ? "✓" : "✗";
	const tone = group.passed === null ? "warn" : group.passed ? "ok" : "bad";
	return (
		<section className="phase">
			<div className="phase-head">
				<span className={tone}>{mark}</span>
				<strong>{group.name}</strong>
				<span className="owner">{group.owner}</span>
				<span className="owner">
					{group.attempts} attempt{group.attempts === 1 ? "" : "s"}
				</span>
				{/* Width is relative to the longest phase: which phase ate the run
				    is the first question a waterfall has to answer. */}
				<div className="bar" style={{ width: `${widest > 0 ? Math.max((ms / widest) * 240, 3) : 3}px` }} />
				<span className="owner">{(ms / 1000).toFixed(1)}s</span>
				{group.tokens > 0 && <span className="owner">{group.tokens.toLocaleString()} tok</span>}
			</div>
			<div className="rows">
				{group.events.map(event => (
					<div className="row" key={event.seq}>
						<span className="kind">{event.kind}</span>
						<span className={toneOf(event)}>{describe(event)}</span>
						<span>{event.attempt > 0 ? `#${event.attempt}` : ""}</span>
					</div>
				))}
			</div>
		</section>
	);
}

export function App() {
	const [runs, setRuns] = useState<RunSummary[]>([]);
	const [selected, setSelected] = useState<string | null>(null);
	const [detail, setDetail] = useState<RunDetail | null>(null);

	useEffect(() => {
		void fetch("/api/runs")
			.then(async response => (await response.json()) as RunSummary[])
			.then(loaded => {
				setRuns(loaded);
				setSelected(current => current ?? loaded[0]?.adwId ?? null);
			});
	}, []);

	useEffect(() => {
		if (!selected) return;
		void fetch(`/api/runs/${selected}`)
			.then(async response => (await response.json()) as RunDetail)
			.then(setDetail);
	}, [selected]);

	const phases = detail ? groupPhases(detail.timeline) : [];
	const widest = Math.max(1, ...phases.map(group => group.endedAt - group.startedAt));

	return (
		<div className="shell">
			<nav className="runs">
				<h1>runs · {runs.length}</h1>
				{runs.map(run => (
					<button
						type="button"
						className="run"
						key={run.adwId}
						aria-current={run.adwId === selected}
						onClick={() => setSelected(run.adwId)}
					>
						<div className="run-name">
							<strong>{run.workflow}</strong>
							<span className={run.accepted === null ? "warn" : run.accepted ? "ok" : "bad"}>
								{run.accepted === null ? "unsettled" : run.accepted ? "accepted" : "rejected"}
							</span>
						</div>
						<div className="run-meta">
							{run.phases} phase{run.phases === 1 ? "" : "s"} · {(run.durationMs / 1000).toFixed(1)}s
							{run.resumed ? " · resumed" : ""}
						</div>
						<div className="run-meta">{run.adwId}</div>
					</button>
				))}
				{runs.length === 0 ? (
					<p className="empty">No runs yet. Try /adw &lt;workflow&gt; &lt;request&gt;.</p>
				) : null}
			</nav>
			<main className="detail">
				{detail ? (
					<>
						<header>
							<h2>{detail.workflow}</h2>
							<span className={`verdict ${detail.accepted === null ? "warn" : detail.accepted ? "ok" : "bad"}`}>
								{detail.accepted === null ? "unsettled" : detail.accepted ? "accepted" : "rejected"}
							</span>
							{detail.resumed ? <span className="verdict warn">resumed</span> : null}
						</header>
						<div className="run-meta">
							{detail.adwId} · {detail.events} events · {(detail.durationMs / 1000).toFixed(1)}s
							{detail.tokens > 0 ? ` · ${detail.tokens.toLocaleString()} tok` : null}
							{/* A floor, not a total: some providers report no usage at all. */}
							{detail.unaccountedPhases > 0
								? ` · ${detail.unaccountedPhases} phase${detail.unaccountedPhases === 1 ? "" : "s"} unaccounted`
								: null}
						</div>
						{phases.map(group => (
							<Phase group={group} widest={widest} key={`${group.name}-${group.startedAt}`} />
						))}
					</>
				) : (
					<p className="empty">Select a run.</p>
				)}
			</main>
		</div>
	);
}
