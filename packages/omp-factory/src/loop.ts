/**
 * Bounded attempt loop: builder -> candidate -> scope -> gate -> correct|accept.
 * The producer is injectable so the boundary logic is provable without a model;
 * production wires it to runSubprocess / runSubagentFollowUpTurn.
 */

export interface Candidate {
	changedFiles: string[];
	label: string;
	/** Builder process outcome; nonzero never accepts. */
	exitCode?: number;
	output?: string;
	/**
	 * Files the builder claims to have produced. Compared against the captured
	 * change-set when present; absent means "no claim to check".
	 */
	declaredArtifacts?: string[];
	/** Envelope contract violation; present means the builder owed a parseable answer and did not give one. */
	envelopeViolation?: string;
	/** The builder's own verdict. `fail` never accepts, however green the gates. */
	selfReportedStatus?: "success" | "fail";
	/** Builder's one-line account, quoted into rejection evidence. */
	summary?: string;
}
export type VerifyOutcome = { accepted: true } | { accepted: false; evidence: string };

export interface LoopResult {
	status: "accepted" | "rejected";
	attempts: number;
	evidence: string[];
	/**
	 * Set when the loop stopped short of its budget. `resource` means the
	 * host failed, not the work: the remaining attempts were left unspent
	 * on purpose and a retry is a scheduling decision, not a correction.
	 */
	halted?: "resource";
}

export async function runAttemptLoop(options: {
	maxAttempts: number;
	produce: (attempt: number, evidence: string | undefined) => Promise<Candidate>;
	verify: (candidate: Candidate) => Promise<VerifyOutcome>;
	/**
	 * Tells a host failure from one an agent could fix. A run killed by the
	 * machine did not fail semantically, and asking an agent to correct code
	 * that never executed spends the budget on a question nobody asked.
	 * Absent means every failure is treated as semantic — the safe default,
	 * since a misread semantic failure would skip a correction that was owed.
	 */
	classify?: (text: string) => "semantic" | "resource";
}): Promise<LoopResult> {
	const { maxAttempts, produce, verify, classify } = options;
	if (!Number.isInteger(maxAttempts) || maxAttempts < 1) {
		return { status: "rejected", attempts: 0, evidence: ["maxAttempts must be an integer >= 1"] };
	}
	const evidence: string[] = [];
	for (let attempt = 1; attempt <= maxAttempts; attempt++) {
		const candidate = await produce(attempt, attempt === 1 ? undefined : evidence.at(-1));
		const outcome = await verify(candidate);
		if (outcome.accepted) {
			return { status: "accepted", attempts: attempt, evidence };
		}
		evidence.push(`attempt ${attempt} (${candidate.label}): ${outcome.evidence}`);
		// A resource failure ends the loop instead of consuming the rest of
		// the budget: the next attempt would meet the same exhausted host,
		// and the correction it carries would describe a fault the producer
		// cannot address.
		if (classify?.(outcome.evidence) === "resource") {
			return { status: "rejected", attempts: attempt, evidence, halted: "resource" };
		}
	}
	return { status: "rejected", attempts: maxAttempts, evidence };
}
