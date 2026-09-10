/**
 * Multi-phase runner: dependency edges mean "an accepted producer version is
 * required", never "the predecessor ran". A phase dispatches only when every
 * dependency has an accepted version; a permanently failed producer keeps its
 * descendants blocked while unrelated branches still finish.
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
	order: string[];
	phases: PhaseOutcome[];
}

function digestOf(candidate: Candidate): string {
	return Bun.hash(JSON.stringify([candidate.changedFiles, candidate.summary ?? ""])).toString(16);
}

/** Validate the graph before anything runs: unknown names and cycles fail the file, not the run. */
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
	return deps;
}

export async function runGraph(request: GraphRequest): Promise<GraphResult> {
	const deps = validate(request.phases);
	const waves = buildWaves(Object.keys(deps), deps);
	const store = new VersionStore();
	const accepted = new Set<string>();
	const outcomes: PhaseOutcome[] = [];
	const order: string[] = [];

	for (const wave of waves) {
		for (const name of wave) {
			const phase = request.phases.find(candidate => candidate.name === name);
			if (!phase) continue;
			const unmet = (phase.dependsOn ?? []).filter(dep => !accepted.has(dep));
			if (unmet.length > 0) {
				outcomes.push({
					phase: name,
					status: "blocked",
					attempts: 0,
					evidence: [`blocked: dependencies without an accepted version: ${unmet.join(", ")}`],
					inputs: [],
				});
				continue;
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
			let lastCandidate: Candidate | undefined;
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
			outcomes.push({
				phase: name,
				status: result.status,
				attempts: result.attempts,
				evidence: result.evidence,
				inputs,
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
		}
	}

	return {
		status: outcomes.every(outcome => outcome.status === "accepted") ? "accepted" : "failed",
		order,
		phases: outcomes,
	};
}
