/**
 * Multi-phase runner: dependency edges mean "an accepted producer version is
 * required", never "the predecessor ran". A phase dispatches only when every
 * dependency has an accepted version; a permanently failed producer keeps its
 * descendants blocked while unrelated branches still finish.
 *
 * Readers in a wave run concurrently. Writers never overlap — this runner
 * shares one workspace, so a single writer token is the only safe contract
 * short of one isolated worktree per writer.
 */
import { buildWaves, type PhaseDeps, VersionStore } from "./dag";
import type { FileAssertion } from "./gates";
import type { Candidate } from "./loop";
import type { ReviewVerdict } from "./review";
import { runWorkflow, type WorkflowRequest } from "./workflow";

/** One resolved input handed to a consumer: which producer, which version. */
export interface SelectedInput {
	phase: string;
	version: number;
	digest: string;
	artifacts: string[];
}

export interface GraphPhase {
	name: string;
	dependsOn?: string[];
	/** Producers whose accepted output this phase consumes; must be dependencies. */
	inputs?: string[];
	scope: string[];
	assertions: FileAssertion[];
	gateCommand?: string[];
	requireArtifacts?: boolean;
	/**
	 * This phase mutates the shared workspace. Writers never overlap: readers
	 * in a wave run concurrently, writers take a single token in wave order.
	 * Parallel writers would need one isolated worktree each, which this
	 * runner does not create.
	 */
	writes?: boolean;
	/**
	 * A coherent rejection revisits an earlier writer instead of refusing
	 * outright. The target must be a transitive dependency; `maxRevisions`
	 * bounds the loop and never replenishes the target's own attempts.
	 */
	onReject?: { to: string; maxRevisions: number };
	maxAttempts: number;
	produce: (context: {
		attempt: number;
		correction: string | undefined;
		inputs: SelectedInput[];
	}) => Promise<Candidate>;
	review?: (candidate: Candidate) => Promise<ReviewVerdict>;
}

export interface GraphRequest {
	workflowId: string;
	runDir: string;
	workspace: string;
	phases: GraphPhase[];
	integrate?: WorkflowRequest["integrate"];
}

export interface PhaseOutcome {
	phase: string;
	status: "accepted" | "rejected" | "blocked";
	attempts: number;
	evidence: string[];
	/** Exact producer versions this phase consumed; recorded for provenance. */
	inputs: SelectedInput[];
}

export interface GraphResult {
	status: "accepted" | "failed";
	/** Dispatch order; a revision appends the re-run phases again. */
	order: string[];
	phases: PhaseOutcome[];
	/** Peak number of writer phases in flight; the single-writer invariant pins this at 1. */
	peakConcurrentWriters: number;
}

function digestOf(candidate: Candidate): string {
	return Bun.hash(JSON.stringify([candidate.changedFiles, candidate.summary ?? ""])).toString(16);
}

/** Transitive dependencies of `name`, used to validate revision routes. */
function ancestors(deps: PhaseDeps, name: string): Set<string> {
	const seen = new Set<string>();
	const stack = [...(deps[name] ?? [])];
	while (stack.length > 0) {
		const current = stack.pop();
		if (current === undefined || seen.has(current)) continue;
		seen.add(current);
		stack.push(...(deps[current] ?? []));
	}
	return seen;
}

/** `target` plus everything that transitively depends on it. */
function closure(deps: PhaseDeps, target: string): Set<string> {
	const affected = new Set([target]);
	let grew = true;
	while (grew) {
		grew = false;
		for (const [name, list] of Object.entries(deps)) {
			if (affected.has(name)) continue;
			if (list.some(dep => affected.has(dep))) {
				affected.add(name);
				grew = true;
			}
		}
	}
	return affected;
}

/** Validate the graph before anything runs: unknown names, bad routes and cycles fail the file, not the run. */
function validate(phases: GraphPhase[]): PhaseDeps {
	const names = new Set(phases.map(phase => phase.name));
	if (names.size !== phases.length) throw new Error("duplicate phase name");
	const deps: PhaseDeps = {};
	for (const phase of phases) {
		for (const dep of phase.dependsOn ?? []) {
			if (!names.has(dep)) throw new Error(`phase "${phase.name}" depends on unknown phase "${dep}"`);
		}
		for (const input of phase.inputs ?? []) {
			if (!(phase.dependsOn ?? []).includes(input)) {
				throw new Error(`phase "${phase.name}" consumes "${input}" without depending on it`);
			}
		}
		deps[phase.name] = [...(phase.dependsOn ?? [])];
	}
	// Throws on cycles, naming every phase in the loop.
	buildWaves(Object.keys(deps), deps);
	for (const phase of phases) {
		const route = phase.onReject;
		if (!route) continue;
		if (!Number.isInteger(route.maxRevisions) || route.maxRevisions < 1) {
			throw new Error(`phase "${phase.name}" has a non-positive maxRevisions`);
		}
		if (!ancestors(deps, phase.name).has(route.to)) {
			throw new Error(`phase "${phase.name}" cannot revise "${route.to}": not a transitive dependency`);
		}
	}
	return deps;
}

export async function runGraph(request: GraphRequest): Promise<GraphResult> {
	const deps = validate(request.phases);
	const waves = buildWaves(Object.keys(deps), deps);
	const byName = new Map(request.phases.map(phase => [phase.name, phase]));
	const store = new VersionStore();
	const accepted = new Set<string>();
	const outcomes = new Map<string, PhaseOutcome>();
	const order: string[] = [];
	const revisionsUsed = new Map<string, number>();
	let writersInFlight = 0;
	let peakConcurrentWriters = 0;

	/** Run one phase to a terminal outcome; blocked phases never dispatch. */
	const runPhase = async (name: string): Promise<PhaseOutcome> => {
		const phase = byName.get(name);
		if (!phase) throw new Error(`unknown phase "${name}"`);
		const unmet = (phase.dependsOn ?? []).filter(dep => !accepted.has(dep));
		if (unmet.length > 0) {
			return {
				phase: name,
				status: "blocked",
				attempts: 0,
				evidence: [`blocked: dependencies without an accepted version: ${unmet.join(", ")}`],
				inputs: [],
			};
		}
		// Selection happens at dispatch and is recorded: "which evidence did
		// this attempt see" has exactly one answer.
		const inputs: SelectedInput[] = [];
		for (const producer of phase.inputs ?? []) {
			const selected = store.select(producer);
			if (!selected) throw new Error(`input "${producer}" for "${name}" has no accepted version`);
			inputs.push({
				phase: selected.phase,
				version: selected.version,
				digest: selected.digest,
				artifacts: selected.artifacts,
			});
		}
		order.push(name);
		if (phase.writes) {
			writersInFlight += 1;
			peakConcurrentWriters = Math.max(peakConcurrentWriters, writersInFlight);
		}
		let lastCandidate: Candidate | undefined;
		try {
			const result = await runWorkflow({
				workflowId: request.workflowId,
				phase: name,
				runDir: request.runDir,
				workspace: request.workspace,
				scope: phase.scope,
				assertions: phase.assertions,
				gateCommand: phase.gateCommand,
				requireArtifacts: phase.requireArtifacts,
				maxAttempts: phase.maxAttempts,
				integrate: request.integrate,
				produce: async (attempt, correction) => {
					lastCandidate = await phase.produce({ attempt, correction, inputs });
					return lastCandidate;
				},
				review: phase.review,
			});
			if (result.status === "accepted" && lastCandidate) {
				accepted.add(name);
				const history = store.select(name);
				store.accept({
					phase: name,
					version: (history?.version ?? 0) + 1,
					digest: digestOf(lastCandidate),
					artifacts: lastCandidate.declaredArtifacts ?? lastCandidate.changedFiles,
				});
			}
			return { phase: name, status: result.status, attempts: result.attempts, evidence: result.evidence, inputs };
		} finally {
			if (phase.writes) writersInFlight -= 1;
		}
	};

	/**
	 * A rejection with a usable route re-opens its target's closure: the
	 * revised producer and every dependent lose their acceptance so the next
	 * pass re-runs them against the new version.
	 */
	const takeRevision = (name: string): string | null => {
		const route = byName.get(name)?.onReject;
		if (!route) return null;
		const used = revisionsUsed.get(name) ?? 0;
		if (used >= route.maxRevisions) return null;
		revisionsUsed.set(name, used + 1);
		for (const affected of closure(deps, route.to)) {
			accepted.delete(affected);
			outcomes.delete(affected);
		}
		return route.to;
	};

	let revising = true;
	while (revising) {
		revising = false;
		for (const wave of waves) {
			const pending = wave.filter(name => !outcomes.has(name));
			if (pending.length === 0) continue;
			const readers = pending.filter(name => byName.get(name)?.writes !== true);
			const writers = pending.filter(name => byName.get(name)?.writes === true);
			const results = await Promise.all(readers.map(name => runPhase(name)));
			for (const outcome of results) outcomes.set(outcome.phase, outcome);
			for (const name of writers) {
				const outcome = await runPhase(name);
				outcomes.set(outcome.phase, outcome);
			}
			const rejected = pending.find(name => outcomes.get(name)?.status === "rejected");
			if (rejected && takeRevision(rejected) !== null) {
				revising = true;
				break;
			}
		}
	}

	const phases = [...outcomes.values()];
	return {
		status: phases.every(outcome => outcome.status === "accepted") ? "accepted" : "failed",
		order,
		phases,
		peakConcurrentWriters,
	};
}
