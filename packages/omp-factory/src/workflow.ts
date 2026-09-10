/**
 * Workflow driver: owns acceptance end to end.
 * builder -> exit check -> scope -> assertions -> gate command -> review -> ledger.
 * The producer is injectable; production wires it to runSubprocess/follow-up.
 */
import { evaluateFileAssertions, type FileAssertion, runGateCommand } from "./gates";
import { applyIntegration, commitIntegration, prepareIntegration, revertIntegration } from "./integrate";
import { appendEvent } from "./ledger";
import { type Candidate, runAttemptLoop } from "./loop";
import { combineGateAndReview, type ReviewVerdict } from "./review";
import { matchesScopeGlob, verifyScope } from "./scope";
import type { PhaseGuard } from "./write-guard";

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
	/** Bounds the gate command. Absent falls back to the gate runner's default. */
	gateTimeoutMs?: number;
	/**
	 * Compare declared artifacts against the captured change-set. Off unless
	 * the phase declared `diff_matches_claims`: holding a phase to a gate it
	 * never asked for rejects honest builders.
	 */
	matchClaims?: boolean;
	/** Paths exempt from that comparison — lockfiles a build rewrites unbidden. */
	undeclaredIgnore?: string[];
	/**
	 * This phase asserts it produces files. An envelope declaring none clears
	 * no gate vacuously — claiming nothing is a rejection, not a pass.
	 */
	requireArtifacts?: boolean;
	/**
	 * Gates evaluated against the envelope's declared artifacts rather than a
	 * static file list — the workflow's `artifacts_exist`, `files_non_empty`
	 * and `json_parses`.
	 */
	artifactChecks?: { exist?: boolean; nonEmpty?: boolean; jsonParses?: boolean };
	/** Paths no phase may change, whatever its scope allows. */
	protectedGlobs?: string[];
	maxAttempts: number;
	/**
	 * Journal accepted change-sets through the integration owner. Absent means
	 * "verify only": acceptance without landing. Deletions fail closed until
	 * the journal expresses them. `root` defaults to `workspace`; an isolated
	 * writer verifies in its own copy and lands in the shared root.
	 * `serialize` wraps the whole prepare/apply/commit so concurrent writers
	 * never interleave on that root.
	 */
	integrate?: {
		journalDir: string;
		base: string;
		root?: string;
		serialize?: <T>(landing: () => Promise<T>) => Promise<T>;
		/**
		 * Refuses a landing whose base moved. An isolated writer's copy is
		 * taken before it produces; if a sibling lands one of the same files
		 * in the meantime, this writer's content derives from a tree that no
		 * longer exists and applying it would erase an accepted change.
		 * Checked and recorded inside `serialize`, so the pair is atomic.
		 */
		guard?: {
			assert: (files: string[]) => string | null;
			record: (files: string[]) => void;
		};
	};
	/**
	 * Rolls back what an attempt wrote outside its scope. `begin` runs before
	 * the producer, `settle` once it returns: refusing a candidate leaves the
	 * bytes on disk, and the next attempt would inherit them.
	 */
	guard?: PhaseGuard;
	/**
	 * Verify the tree the landing produced, not the one it was built in.
	 *
	 * Runs after APPLIED and before COMMITTED, and only when the landing root
	 * differs from the workspace: verifying the same tree twice buys nothing.
	 * A failure reverts the landing, so an unverifiable delivered tree is
	 * never left behind.
	 */
	verifyDelivered?: (root: string) => Promise<{ ok: true } | { ok: false; evidence: string }>;
	/**
	 * Separates a host failure from one the producer could fix, so an
	 * exhausted machine does not consume the correction budget. Injected:
	 * the taxonomy lives in the harness (`task/admission`), which already
	 * knows a provider rate limit from a bad tool schema.
	 */
	classifyFailure?: (text: string) => "semantic" | "resource";
	produce: (attempt: number, evidence: string | undefined) => Promise<Candidate>;
	review?: (candidate: Candidate) => Promise<ReviewVerdict>;
}
export interface WorkflowResult {
	status: "accepted" | "rejected";
	attempts: number;
	evidence: string[];
}

/**
 * Land an accepted candidate through the serialized journal. Reads current
 * file contents (the captured change-set names them); a file that vanished
 * since capture fails closed until the journal expresses deletions.
 */
async function integrateCandidate(
	request: WorkflowRequest,
	candidate: Candidate,
): Promise<{ ok: true } | { ok: false; evidence: string }> {
	const spec = request.integrate;
	if (!spec) return { ok: true };
	const { workflowId, phase, runDir, workspace } = request;
	const root = spec.root ?? workspace;
	const content: Record<string, string> = {};
	for (const file of candidate.changedFiles) {
		try {
			content[file] = await Bun.file(`${workspace}/${file}`).text();
		} catch {
			return { ok: false, evidence: `deleted files unsupported by integration journal: ${file}` };
		}
	}
	const land = async (): Promise<void> => {
		const moved = spec.guard?.assert(candidate.changedFiles);
		if (moved) throw new Error(moved);
		const journal = await prepareIntegration(root, spec.base, content);
		await appendEvent(runDir, workflowId, "IntegrationPrepared", { phase, digest: journal.patchDigest });
		await appendEvent(runDir, workflowId, "IntegrationApplying", { phase, digest: journal.patchDigest });
		const applied = await applyIntegration(root, spec.journalDir, journal);
		await appendEvent(runDir, workflowId, "IntegrationApplied", { phase, digest: journal.patchDigest });
		// The candidate was verified in the workspace it was produced in. When
		// that is not the tree the change landed on, nothing has yet checked
		// the tree the operator actually receives: two landings can each be
		// valid alone and broken together. Re-run the deterministic gate
		// against the delivered root before committing.
		if (request.verifyDelivered && root !== workspace) {
			const delivered = await request.verifyDelivered(root);
			if (!delivered.ok) {
				const undo = await revertIntegration(root, spec.journalDir, applied);
				await appendEvent(runDir, workflowId, "IntegrationReverted", {
					phase,
					digest: journal.patchDigest,
					evidence: [delivered.evidence, `reverted: ${undo.reverted.join(", ") || "nothing"}`],
				});
				throw new Error(
					undo.skipped.length > 0
						? `${delivered.evidence}; revert incomplete, still applied: ${undo.skipped.join(", ")}`
						: delivered.evidence,
				);
			}
		}
		await commitIntegration(spec.journalDir, applied);
		spec.guard?.record(candidate.changedFiles);
		await appendEvent(runDir, workflowId, "IntegrationCommitted", { phase, digest: journal.patchDigest });
	};
	try {
		await (spec.serialize ? spec.serialize(land) : land());
		return { ok: true };
	} catch (error) {
		return {
			ok: false,
			evidence: `integration failed: ${error instanceof Error ? error.message : String(error)}`,
		};
	}
}
export async function runWorkflow(request: WorkflowRequest): Promise<WorkflowResult> {
	const { workflowId, phase, runDir } = request;
	await appendEvent(runDir, workflowId, "PhaseStarted", { phase });
	let startedAttempts = 0;
	let result: WorkflowResult;
	try {
		result = await runAttemptLoop({
			maxAttempts: request.maxAttempts,
			classify: request.classifyFailure,
			produce: async (attempt, evidence) => {
				await appendEvent(runDir, workflowId, "AttemptStarted", { phase, attempt });
				startedAttempts += 1;
				// The boundary is taken before the producer writes anything, so
				// a restore returns the tree to what this attempt inherited.
				request.guard?.begin();
				return request.produce(attempt, evidence);
			},
			verify: async candidate => {
				// Settle first: the guard is the authority on what changed and
				// the only thing that can undo it. Every later check reads a
				// tree it has already restored.
				if (request.guard) {
					const report = request.guard.settle(request.scope, request.protectedGlobs ?? []);
					if (report.unrecoverable.length > 0) {
						return {
							accepted: false as const,
							evidence:
								`unauthorized changes could not be restored: ${report.unrecoverable.join(", ")}` +
								(report.patchPath ? ` (diff preserved at ${report.patchPath})` : ""),
						};
					}
					if (report.unauthorized.length > 0) {
						return {
							accepted: false as const,
							evidence: `scope violation: ${report.unauthorized.join(", ")} (rolled back)`,
						};
					}
				}
				if (candidate.exitCode !== undefined && candidate.exitCode !== 0) {
					return {
						accepted: false as const,
						evidence: `builder failed with exit ${candidate.exitCode}: ${(candidate.output ?? "").slice(0, 500)}`,
					};
				}
				if (candidate.envelopeViolation) {
					return { accepted: false as const, evidence: `envelope violation: ${candidate.envelopeViolation}` };
				}
				if (candidate.selfReportedStatus === "fail") {
					return {
						accepted: false as const,
						evidence: `builder reported failure: ${candidate.summary ?? "no summary"}`,
					};
				}
				if (request.requireArtifacts && (candidate.declaredArtifacts?.length ?? 0) === 0) {
					return {
						accepted: false as const,
						evidence: "phase requires artifacts but the envelope declared none",
					};
				}
				const scope = verifyScope(candidate.changedFiles, request.scope, request.protectedGlobs ?? []);
				if (!scope.ok) {
					return { accepted: false as const, evidence: `scope violation: ${scope.violations.join(", ")}` };
				}
				const artifactChecks = request.artifactChecks;
				if (artifactChecks) {
					const declared = candidate.declaredArtifacts ?? [];
					const checks: FileAssertion[] = [];
					for (const file of declared) {
						if (artifactChecks.exist || artifactChecks.nonEmpty) checks.push({ type: "file_exists", file });
						if (artifactChecks.nonEmpty) checks.push({ type: "file_non_empty", file });
						if (artifactChecks.jsonParses && file.endsWith(".json")) checks.push({ type: "json_parses", file });
					}
					const report = await evaluateFileAssertions(checks, request.workspace);
					if (!report.passed) {
						return { accepted: false as const, evidence: report.failures.join("; ") };
					}
				}
				if (request.matchClaims && candidate.declaredArtifacts !== undefined) {
					const ignore = request.undeclaredIgnore ?? [];
					const exempt = (file: string): boolean => ignore.some(glob => matchesScopeGlob(glob, file));
					const actual = new Set(candidate.changedFiles);
					const undeclared = candidate.changedFiles.filter(
						file => !candidate.declaredArtifacts?.includes(file) && !exempt(file),
					);
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
					const gate = await runGateCommand(request.gateCommand, request.workspace, request.gateTimeoutMs);
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
				if (request.integrate) {
					const landed = await integrateCandidate(request, candidate);
					if (!landed.ok) return { accepted: false as const, evidence: landed.evidence };
				}
				// The version ordinal is the graph's: it owns the store that
				// knows what this phase produced before. A per-call counter
				// here reported 1 for every acceptance, including revisions.
				await appendEvent(runDir, workflowId, "PhaseAccepted", { phase });
				return { accepted: true as const };
			},
		});
	} catch (error) {
		const evidence = [`producer threw: ${error instanceof Error ? error.message : String(error)}`];
		await appendEvent(runDir, workflowId, "PhaseFailed", { phase, evidence });
		return { status: "rejected", attempts: startedAttempts, evidence };
	}
	// Only phase-scoped events belong here. Whether the WORKFLOW is accepted is
	// the graph's call: one phase passing decides nothing about its siblings.
	if (result.status !== "accepted") {
		await appendEvent(runDir, workflowId, "PhaseFailed", { phase, evidence: result.evidence });
	}
	return result;
}
