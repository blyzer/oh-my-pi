import { useEffect, useMemo, useState } from "react";
import type { RunDetail, RunSummary, TraceEvent, WorkflowPhase } from "./trace";

/** Events grouped into the phase they belong to, in trace order. */
interface PhaseGroup {
	id: number;
	name: string;
	owner: string;
	kind: WorkflowPhase["kind"];
	attempt: number;
	tokens: number;
	passed: boolean | null;
	invalidated: boolean;
	startedAt: number;
	endedAt: number;
	inputs: { event: TraceEvent; producerId?: number }[];
	events: TraceEvent[];
}

/** Name-addressed instances keep interleaved events and revisions causal. */
function groupPhases(timeline: TraceEvent[], workflow: WorkflowPhase[]): PhaseGroup[] {
	const groups: PhaseGroup[] = [];
	const current = new Map<string, PhaseGroup>();
	const byName = new Map(workflow.map(phase => [phase.name, phase]));
	const graph = new Map(
		workflow.map((phase, index) => [phase.name, phase.dependsOn ?? (index > 0 ? [workflow[index - 1]!.name] : [])]),
	);
	const explicitInvalidation = timeline.some(event => event.kind === "phase_invalidated");
	const invalidate = (name: string, event: TraceEvent) => {
		const group = current.get(name);
		if (!group) return;
		group.invalidated = true;
		if (group.passed === null) group.endedAt = event.ts;
	};
	for (const event of timeline) {
		if (!event.phase) continue;
		if (event.kind === "phase_invalidated") {
			invalidate(event.phase, event);
			current.get(event.phase)?.events.push(event);
			continue;
		}
		if (event.kind === "review_revision" || event.kind === "phase_rewound") {
			if (!explicitInvalidation) {
				// Legacy traces lack per-name invalidation. Reconstruct the actual
				// closure from the saved declaration graph, never a timeline suffix.
				const invalidated = new Set([event.phase]);
				let changed = true;
				while (changed) {
					changed = false;
					for (const [name, deps] of graph) {
						if (!invalidated.has(name) && deps.some(dep => invalidated.has(dep))) {
							invalidated.add(name);
							changed = true;
						}
					}
				}
				for (const name of invalidated) invalidate(name, event);
			}
			const source = current.get(event.owner);
			if (source) {
				source.events.push(event);
				source.endedAt = event.ts;
				if (event.kind === "phase_rewound") source.passed = false;
			}
			continue;
		}
		let group = current.get(event.phase);
		const starts = event.kind === "phase_started" || event.kind === "phase_retry";
		if (starts) {
			group = {
				id: event.seq,
				name: event.phase,
				owner: event.owner,
				kind: byName.get(event.phase)?.kind ?? "agent",
				attempt: event.attempt,
				tokens: 0,
				passed: null,
				invalidated: false,
				startedAt: event.ts,
				endedAt: event.ts,
				inputs: [],
				events: [],
			};
			groups.push(group);
			current.set(event.phase, group);
		}
		if (!group) continue;
		// Input producers and panel seats are not this phase's writer.
		if (event.kind === "input_selected") {
			const producer = current.get(event.owner);
			group.inputs.push({ event, producerId: producer?.passed && !producer.invalidated ? producer.id : undefined });
		}
		if (group.passed === null && !group.invalidated) group.endedAt = event.ts;
		if (event.kind === "phase_tokens" || event.kind === "panel_opinion") group.tokens += event.value;
		group.events.push(event);
		if (event.kind === "phase_finished" || event.kind === "phase_rejected") {
			group.passed = event.kind === "phase_finished" && event.ok;
			group.endedAt = event.ts;
		}
	}
	return groups;
}

function describe(event: TraceEvent): string {
	switch (event.kind) {
		case "gate_check":
			return `${event.gate}: ${event.detail}`;
		case "panel_opinion":
			return `${event.owner}${event.detail ? ` (${event.detail})` : ""} — ${event.value > 0 ? `${event.value.toLocaleString()} tokens` : "usage unavailable"}`;
		case "phase_tokens":
			// A model turn cannot cost nothing; a zero is a provider that
			// reported no usage, and saying "0 tokens" would read as free.
			return event.ok
				? `${event.owner}${event.detail ? ` (${event.detail})` : ""} — ${event.value.toLocaleString()} tokens${event.attempt > 1 ? ` (attempt ${event.attempt})` : ""}`
				: `${event.owner}${event.detail ? ` (${event.detail})` : ""} — provider reported no usage`;
		case "phase_rejected":
			return `${event.value} violation${event.value === 1 ? "" : "s"}${event.detail ? ` · ${event.detail}` : ""}`;
		case "input_selected":
			return `consumes ${event.owner} v${event.value}`;
		case "review_revision":
			return `Revision ${event.value}: ${event.owner} → ${event.phase}`;
		case "phase_rewound":
			return `${event.owner} → ${event.phase}${event.detail ? ` · ${event.detail}` : ""}`;
		case "phase_invalidated":
			return event.owner ? `superseded by ${event.owner}` : "interrupted attempt reset on resume";
		case "review_exhausted":
			return event.detail;
		default:
			return event.detail;
	}
}

function toneOf(event: TraceEvent): string {
	if (event.kind === "gate_check" || event.kind === "phase_finished") return event.ok ? "ok" : "bad";
	if (event.kind === "phase_rejected" || event.kind === "phase_retry") return "warn";
	if (event.kind === "review_revision" || event.kind === "phase_rewound") return "warn";
	if (event.kind === "review_exhausted") return "bad";
	return "";
}

function Phase({ group, origin, end }: { group: PhaseGroup; origin: number; end: number }) {
	const endedAt = group.passed === null && !group.invalidated ? end : group.endedAt;
	const ms = Math.max(endedAt - group.startedAt, 0);
	const span = Math.max(end - origin, 1);
	const mark = group.invalidated ? "—" : group.passed === null ? "◌" : group.passed ? "✓" : "✗";
	const tone = group.invalidated || group.passed === null ? "warn" : group.passed ? "ok" : "bad";
	return (
		<section className="phase" id={`phase-${group.id}`}>
			<div className="phase-head">
				<span className={tone}>{mark}</span>
				<strong>{group.name}</strong>
				{group.invalidated && <span className="warn">invalidated</span>}
				<span className="owner">{group.owner}</span>
				<span className="owner">attempt {group.attempt}</span>
				<span className="owner">
					{(ms / 1000).toFixed(1)}s · +{((group.startedAt - origin) / 1000).toFixed(1)}s from start
				</span>
				{group.tokens > 0 && <span className="owner">{group.tokens.toLocaleString()} tok</span>}
			</div>
			<div
				aria-label={`${group.name}: starts ${group.startedAt - origin}ms into run, duration ${ms}ms`}
				style={{ position: "relative", height: 14, margin: "8px 14px", background: "var(--line)" }}
			>
				<div
					className={`bar ${group.kind === "code" ? "code" : ""}`}
					style={{
						position: "absolute",
						top: 4,
						left: `${((group.startedAt - origin) / span) * 100}%`,
						width: `${(ms / span) * 100}%`,
						opacity: group.invalidated ? 0.35 : 1,
					}}
				/>
			</div>
			{group.inputs.length > 0 ? (
				<div className="rows">
					<span className="owner">Selected inputs: </span>
					{group.inputs.map(({ event, producerId }, index) => (
						<span key={event.seq}>
							{index > 0 ? " · " : ""}
							{producerId === undefined ? (
								`${event.owner} v${event.value}`
							) : (
								<a className="owner" href={`#phase-${producerId}`}>
									{event.owner} v{event.value}
								</a>
							)}
						</span>
					))}
				</div>
			) : null}
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

	const [error, setError] = useState<string | null>(null);

	useEffect(() => {
		let disposed = false;
		let timer: number | undefined;
		const controller = new AbortController();
		const refresh = async () => {
			try {
				const response = await fetch("/api/runs", { signal: controller.signal });
				if (!response.ok) throw new Error(`Run listing failed (${response.status})`);
				const loaded = (await response.json()) as RunSummary[];
				if (disposed) return;
				setRuns(loaded);
				setSelected(current => current ?? loaded[0]?.adwId ?? null);
			} catch (err) {
				if (!disposed) setError(String(err));
			} finally {
				if (!disposed) timer = window.setTimeout(refresh, 2000);
			}
		};
		void refresh();
		return () => {
			disposed = true;
			controller.abort();
			window.clearTimeout(timer);
		};
	}, []);

	useEffect(() => {
		setDetail(null);
		if (!selected) return;
		let disposed = false;
		let timer: number | undefined;
		const controller = new AbortController();
		const refresh = async () => {
			try {
				const response = await fetch(`/api/runs/${encodeURIComponent(selected)}`, { signal: controller.signal });
				if (!response.ok) throw new Error(`Run detail failed (${response.status})`);
				const loaded = (await response.json()) as RunDetail;
				if (disposed) return;
				setDetail(loaded);
				setError(null);
			} catch (err) {
				if (!disposed) setError(String(err));
			} finally {
				if (!disposed) timer = window.setTimeout(refresh, 2000);
			}
		};
		void refresh();
		return () => {
			disposed = true;
			controller.abort();
			window.clearTimeout(timer);
		};
	}, [selected]);

	const phases = useMemo(() => (detail ? groupPhases(detail.timeline, detail.workflowPhases) : []), [detail]);
	const origin = detail?.startedAt ?? 0;
	const end = detail ? Math.max(origin, detail.timeline.at(-1)?.ts ?? origin) : origin;

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
				{error ? (
					<p className="bad" role="alert">
						{error}
					</p>
				) : null}
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
							<Phase group={group} origin={origin} end={end} key={group.id} />
						))}
					</>
				) : (
					<p className="empty">Select a run.</p>
				)}
			</main>
		</div>
	);
}
