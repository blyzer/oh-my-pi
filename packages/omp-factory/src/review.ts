/**
 * Semantic review (WP-10): judgment only, never verification authority.
 * VERIFY=FAIL + REVIEW=ACCEPT is still FAIL. Review runs after gates pass.
 */

export interface ReviewVerdict {
	approved: boolean;
	blockers: string[];
	findings: string[];
}

export type FinalDecision = { accepted: true } | { accepted: false; reason: string; needsRevision: boolean };

export function combineGateAndReview(
	gatePassed: boolean,
	gateEvidence: string[],
	review: ReviewVerdict,
): FinalDecision {
	if (!gatePassed) {
		return {
			accepted: false,
			needsRevision: true,
			reason: `deterministic gate failed; reviewer approval is irrelevant: ${gateEvidence.join("; ")}`,
		};
	}
	if (!review.approved || review.blockers.length > 0) {
		return {
			accepted: false,
			needsRevision: true,
			reason: `reviewer withheld approval: ${review.blockers.join("; ") || "not approved"}`,
		};
	}
	return { accepted: true };
}
