/**
 * Capability routing (WP-11): Factory expresses role requirements; the
 * catalog of available models is injected. No provider or gateway names here.
 */

export type Capability = "reasoning" | "coding" | "review" | "research" | "cheap";

export interface RoleRequirement {
	role: string;
	capability: Capability;
}

export interface ModelCandidate {
	model: string;
	capabilities: readonly Capability[];
	costPerAttempt: number;
}

export function selectModel(requirement: RoleRequirement, candidates: ModelCandidate[]): ModelCandidate | null {
	const eligible = candidates.filter(candidate => candidate.capabilities.includes(requirement.capability));
	if (eligible.length === 0) return null;
	eligible.sort((a, b) => a.costPerAttempt - b.costPerAttempt);
	return eligible[0] ?? null;
}
