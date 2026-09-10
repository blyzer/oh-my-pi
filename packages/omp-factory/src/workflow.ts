/**
 * Workflow driver: owns acceptance end to end.
 * builder -> exit check -> scope -> assertions -> gate command -> review -> ledger.
 * The producer is injectable; production wires it to runSubprocess/follow-up.
 */
import { evaluateFileAssertions, type FileAssertion, runGateCommand } from "./gates";
import { appendEvent } from "./ledger";
import { type Candidate, runAttemptLoop } from "./loop";
import { combineGateAndReview, type ReviewVerdict } from "./review";
import { verifyScope } from "./scope";

export interface WorkflowRequest {
	workflowId: string;
	phase: string;
	/** Directory holding events.jsonl / checkpoint.json. */
	runDir: string;
	/** Workspace root the assertions and scope are evaluated against. */
	workspace: string;
	scope: string[];
	assertions: FileAssertion[];
	gateCommand?: string[];
	maxAttempts: number;
	produce: (attempt: number, evidence: string | undefined) => Promise<Candidate>;
	review?: (candidate: Candidate) => Promise<ReviewVerdict>;
}

export interface WorkflowResult {
	status: "accepted" | "rejected";
	attempts: number;
	evidence: string[];
}

export async function runWorkflow(request: WorkflowRequest): Promise<WorkflowResult> {
	const { workflowId, phase, runDir } = request;
	await appendEvent(runDir, workflowId, "WorkflowStarted", {});
	let version = 0;
	let startedAttempts = 0;
	let result: WorkflowResult;
	try {
		result = await runAttemptLoop({
			maxAttempts: request.maxAttempts,
			produce: async (attempt, evidence) => {
				await appendEvent(runDir, workflowId, "AttemptStarted", { phase, attempt });
				startedAttempts += 1;
				return request.produce(attempt, evidence);
			},
			verify: async candidate => {
				if (candidate.exitCode !== undefined && candidate.exitCode !== 0) {
					return {
						accepted: false as const,
						evidence: `builder failed with exit ${candidate.exitCode}: ${(candidate.output ?? "").slice(0, 500)}`,
					};
				}
				const scope = verifyScope(candidate.changedFiles, request.scope);
				if (!scope.ok) {
					return { accepted: false as const, evidence: `scope violation: ${scope.violations.join(", ")}` };
				}
				if (candidate.declaredArtifacts !== undefined) {
					const actual = new Set(candidate.changedFiles);
					const undeclared = candidate.changedFiles.filter(file => !candidate.declaredArtifacts?.includes(file));
					const missing = candidate.declaredArtifacts.filter(file => !actual.has(file));
					if (undeclared.length > 0 || missing.length > 0) {
						const parts: string[] = [];
						if (undeclared.length > 0) parts.push(`undeclared writes: ${undeclared.join(", ")}`);
						if (missing.length > 0) parts.push(`claimed artifacts absent: ${missing.join(", ")}`);
						return { accepted: false as const, evidence: parts.join("; ") };
					}
				}
				const assertions = await evaluateFileAssertions(request.assertions, request.workspace);
				if (!assertions.passed) {
					return { accepted: false as const, evidence: assertions.failures.join("; ") };
				}
				if (request.gateCommand) {
					const gate = await runGateCommand(request.gateCommand, request.workspace);
					if (gate.exitCode !== 0) {
						return {
							accepted: false as const,
							evidence: `gate exit ${gate.exitCode}: ${gate.output.slice(0, 500)}`,
						};
					}
				}
				if (request.review) {
					const final = combineGateAndReview(true, [], await request.review(candidate));
					if (!final.accepted) return { accepted: false as const, evidence: final.reason };
				}
				version += 1;
				await appendEvent(runDir, workflowId, "VersionAccepted", { phase, version });
				return { accepted: true as const };
			},
		});
	} catch (error) {
		const evidence = [`producer threw: ${error instanceof Error ? error.message : String(error)}`];
		await appendEvent(runDir, workflowId, "PhaseFailed", { phase, evidence });
		await appendEvent(runDir, workflowId, "WorkflowFailed", { phase, evidence });
		return { status: "rejected", attempts: startedAttempts, evidence };
	}
	if (result.status === "accepted") {
		await appendEvent(runDir, workflowId, "WorkflowAccepted", { phase, version });
	} else {
		await appendEvent(runDir, workflowId, "PhaseFailed", { phase, evidence: result.evidence });
		await appendEvent(runDir, workflowId, "WorkflowFailed", { phase, evidence: result.evidence });
	}
	return result;
}
