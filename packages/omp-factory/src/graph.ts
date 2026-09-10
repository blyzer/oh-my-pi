/**
 * Multi-phase runner: dependency edges mean "an accepted producer version is
 * required", never "the predecessor ran". A phase dispatches only when every
 * dependency has an accepted version; a permanently failed producer keeps its
 * descendants blocked while unrelated branches still finish.
 *
 * Readers in a wave run concurrently. Writers never overlap — this runner
 * takes one token at a time on a shared tree, which is the strongest safety
 * short of one isolated worktree per writer.
 */
import { buildWaves, type PhaseDeps, VersionStore } from "./dag";
import type { FileAssertion } from "./gates";
import type { Candidate } from "./loop";
import { appendEvent } from "./ledger";
import type { ReviewVerdict } from "./review";
import { runWorkflow, type WorkflowRequest } from "./workflow";
import type { WriteGuardProvider } from "./write-guard";

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
	/** Bounds the gate command; the workflow's `timeoutMs`. */
	gateTimeoutMs?: number;
	/**
	 * Compare the envelope's declared artifacts against the captured
	 * change-set — the workflow's `diff_matches_claims`. A phase that never
	 * declared the gate is not held to it.
	 */
	matchClaims?: boolean;
	/** Paths exempt from the claims comparison; the workflow's `undeclaredIgnore`. */
	undeclaredIgnore?: string[];
	requireArtifacts?: boolean;
	/** Gates evaluated against the envelope's declared artifacts. */
	artifactChecks?: { exist?: boolean; nonEmpty?: boolean; jsonParses?: boolean };
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
	 * bounds the loop. Each revision re-enters the target with a fresh
	 * attempt budget, so the effective producer bound is the product.
	 */
	onReject?: { to: string; maxRevisions: number };
	maxAttempts: number;
	produce: (context: {
		attempt: number;
		correction: string | undefined;
		inputs: SelectedInput[];
		/** Where this attempt must write: the isolated copy when isolation is on. */
		workspace: string;
	}) => Promise<Candidate>;
	review?: (candidate: Candidate) => Promise<ReviewVerdict>;
}

/**
 * Materialises a private copy of the workspace for one writer. Injected so
 * the runner's contract is testable without a filesystem backend; production
 * passes the adapter over OMP's own isolation lifecycle.
 */
export interface IsolationProvider {
	start(id: string, baseCwd: string): Promise<{ dir: string; stop: () => Promise<void> }>;
}

export interface GraphRequest {
	workflowId: string;
	runDir: string;
	workspace: string;
	phases: GraphPhase[];
	integrate?: WorkflowRequest["integrate"];
	/** Paths no phase may change, whatever its own scope allows. */
	protectedGlobs?: string[];
	/**
	 * Give each writer its own copy of the workspace. Without it writers are
	 * serialized on the shared tree — the only other safe option.
	 */
	isolation?: IsolationProvider;
	/**
	 * Rolls back what a rejected attempt wrote on the shared workspace.
	 * Costly on a large tree — see `write-guard.ts` — so it is injected, not
	 * assumed.
	 */
	writeGuard?: WriteGuardProvider;
	/**
	 * Run un-isolated writers with no guard: a rejected attempt's files stay
	 * on disk and the next attempt inherits them. Explicit because it is a
	 * weaker contract than either alternative, and silence is how a safety
	 * property gets lost.
	 */
	allowUnguardedWrites?: boolean;
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
		deps[phase.name] = [...(phase.dependsOn ?? [])];
	}
	// Throws on cycles, naming every phase in the loop.
	buildWaves(Object.keys(deps), deps);
	for (const phase of phases) {
		// An input must be a transitive dependency: that is what proves the
		// consumed version exists before the consumer dispatches.
		const reachable = ancestors(deps, phase.name);
		for (const input of phase.inputs ?? []) {
			if (!reachable.has(input)) {
				throw new Error(`phase "${phase.name}" consumes "${input}" without depending on it`);
			}
		}
	}
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
	// A writer on the shared tree needs something that can undo a rejected
	// attempt. Isolation keeps the writes off the tree; the guard restores
	// them. Neither is a real configuration, so it has to be asked for.
	if (!request.isolation && !request.writeGuard && !request.allowUnguardedWrites) {
		const writers = request.phases.filter(phase => phase.writes === true).map(phase => phase.name);
		if (writers.length > 0) {
			throw new Error(
				`writer phases on a shared workspace need isolation or a writeGuard: ${writers.join(", ")}. ` +
					"Pass allowUnguardedWrites to accept that a rejected attempt's files stay on disk.",
			);
		}
	}
	const store = new VersionStore();
	await appendEvent(request.runDir, request.workflowId, "WorkflowStarted", {});
	const accepted = new Set<string>();
	const outcomes = new Map<string, PhaseOutcome>();
	const order: string[] = [];
	const revisionsUsed = new Map<string, number>();
	// A rejection's reason, waiting for the phase the route sends it to.
	const corrections = new Map<string, string>();
	let writersInFlight = 0;
	let peakConcurrentWriters = 0;
	// Which landing last wrote each file. A writer records the counter when
	// its workspace is taken; a landing whose files moved past that mark is
	// derived from a tree that no longer exists.
	const landedAt = new Map<string, number>();
	let landingSeq = 0;
	// One landing at a time on the shared root, whatever runs in parallel above it.
	let landingChain: Promise<unknown> = Promise.resolve();
	const landingLock = <T>(landing: () => Promise<T>): Promise<T> => {
		const next = landingChain.then(landing, landing);
		landingChain = next.catch(() => undefined);
		return next;
	};
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
		const isolate = phase.writes === true && request.isolation !== undefined;
		if (phase.writes) {
			writersInFlight += 1;
			peakConcurrentWriters = Math.max(peakConcurrentWriters, writersInFlight);
		}
		let lastCandidate: Candidate | undefined;
		let sandbox: { dir: string; stop: () => Promise<void> } | undefined;
		try {
			if (isolate && request.isolation) {
				sandbox = await request.isolation.start(`${request.workflowId}-${name}`, request.workspace);
			}
			const workspace = sandbox?.dir ?? request.workspace;
			// An isolated writer's sandbox is discarded whole, so nothing it
			// wrote can outlive a rejection; the guard is for the shared tree.
			const guard =
				!isolate && phase.writes === true ? request.writeGuard?.open(name, workspace, request.runDir) : undefined;
			// The mark is taken with the copy: everything landed after this
			// point is invisible to what this writer is about to produce.
			const baseMark = landingSeq;
			// An isolated writer verifies inside its own copy and lands in the
			// shared root; landings take the integration lock so two accepted
			// diffs never interleave on that root.
			const integrate = request.integrate
				? {
						...request.integrate,
						root: request.integrate.root ?? request.workspace,
						serialize: landingLock,
						guard: {
							assert: (files: string[]): string | null => {
								const moved = files.filter(file => (landedAt.get(file) ?? -1) > baseMark);
								return moved.length === 0
									? null
									: `base moved under this writer: ${moved.join(", ")} landed after this workspace was taken`;
							},
							record: (files: string[]): void => {
								landingSeq += 1;
								for (const file of files) landedAt.set(file, landingSeq);
							},
						},
					}
				: undefined;
			const result = await runWorkflow({
				guard,
				workflowId: request.workflowId,
				phase: name,
				runDir: request.runDir,
				workspace,
				scope: phase.scope,
				assertions: phase.assertions,
				artifactChecks: phase.artifactChecks,
				protectedGlobs: request.protectedGlobs,
				gateCommand: phase.gateCommand,
				gateTimeoutMs: phase.gateTimeoutMs,
				matchClaims: phase.matchClaims,
				undeclaredIgnore: phase.undeclaredIgnore,
				requireArtifacts: phase.requireArtifacts,
				maxAttempts: phase.maxAttempts,
				integrate,
				produce: async (attempt, correction) => {
					// Attempt 1 of a revised phase carries the rejection that
					// re-opened it; later attempts carry their own last failure.
					const handed = correction ?? (attempt === 1 ? corrections.get(name) : undefined);
					if (attempt === 1) corrections.delete(name);
					lastCandidate = await phase.produce({ attempt, correction: handed, inputs, workspace });
					return lastCandidate;
				},
				review: phase.review,
			});
			let acceptedVersion: number | undefined;
			if (result.status === "accepted" && lastCandidate) {
				accepted.add(name);
				const history = store.select(name);
				acceptedVersion = (history?.version ?? 0) + 1;
				store.accept({
					phase: name,
					version: acceptedVersion,
					digest: digestOf(lastCandidate),
					artifacts: lastCandidate.declaredArtifacts ?? lastCandidate.changedFiles,
				});
				await appendEvent(request.runDir, request.workflowId, "VersionAccepted", {
					phase: name,
					version: acceptedVersion,
				});
			}
			return { phase: name, status: result.status, attempts: result.attempts, evidence: result.evidence, inputs };
		} finally {
			if (phase.writes) writersInFlight -= 1;
			// Cleanup failure is evidence, not a verdict: an accepted phase
			// that already landed must not be undone by a failed `rm`.
			await sandbox?.stop().catch(() => undefined);
		}
	};

	/**
	 * A rejection with a usable route re-opens its target's closure: the
	 * revised producer and every dependent lose their acceptance so the next
	 * pass re-runs them against the new version. The rejecting phase's
	 * evidence travels with the route — a corrector told only that it failed,
	 * without being told what failed, reproduces the same output and spends
	 * the budget learning nothing.
	 */
	const takeRevision = (name: string): string | null => {
		const route = byName.get(name)?.onReject;
		if (!route) return null;
		const used = revisionsUsed.get(name) ?? 0;
		if (used >= route.maxRevisions) return null;
		revisionsUsed.set(name, used + 1);
		// Read before the closure is cleared: the rejecting phase is itself a
		// descendant of the target, so its outcome is about to be deleted.
		const reason = outcomes.get(name)?.evidence.at(-1);
		for (const affected of closure(deps, route.to)) {
			accepted.delete(affected);
			outcomes.delete(affected);
		}
		corrections.set(route.to, `Phase "${name}" rejected the work that depends on you${reason ? `: ${reason}` : "."}`);
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
			// Concurrent writers are safe only when each holds its own tree;
			// on a shared workspace they take the token one at a time.
			const concurrent = request.isolation ? [...readers, ...writers] : readers;
			const serialized = request.isolation ? [] : writers;
			const results = await Promise.all(concurrent.map(name => runPhase(name)));
			for (const outcome of results) outcomes.set(outcome.phase, outcome);
			for (const name of serialized) {
				const outcome = await runPhase(name);
				outcomes.set(outcome.phase, outcome);
			}
			// Every rejected phase in the wave is offered its route, not just
			// whichever one sorts first: a sibling's exhausted budget must not
			// consume another phase's.
			const rejected = pending.filter(name => outcomes.get(name)?.status === "rejected");
			if (rejected.some(name => takeRevision(name) !== null)) {
				revising = true;
				break;
			}
		}
	}

	const phases = [...outcomes.values()];
	const status = phases.every(outcome => outcome.status === "accepted") ? "accepted" : "failed";
	// One verdict per run: the graph decides the workflow, phases decide themselves.
	await appendEvent(
		request.runDir,
		request.workflowId,
		status === "accepted" ? "WorkflowAccepted" : "WorkflowFailed",
		{ phases: phases.map(outcome => `${outcome.phase}:${outcome.status}`) },
	);
	return { status, order, phases, peakConcurrentWriters };
}
