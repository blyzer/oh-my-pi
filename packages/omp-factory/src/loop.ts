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
}
export type VerifyOutcome = { accepted: true } | { accepted: false; evidence: string };

export interface LoopResult {
	status: "accepted" | "rejected";
	attempts: number;
	evidence: string[];
}

export async function runAttemptLoop(options: {
	maxAttempts: number;
	produce: (attempt: number, evidence: string | undefined) => Promise<Candidate>;
	verify: (candidate: Candidate) => Promise<VerifyOutcome>;
}): Promise<LoopResult> {
	const { maxAttempts, produce, verify } = options;
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
	}
	return { status: "rejected", attempts: maxAttempts, evidence };
}
