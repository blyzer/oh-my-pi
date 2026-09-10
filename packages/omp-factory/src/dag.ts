/**
 * Accepted-version DAG (WP-08): an edge means "accepted producer version
 * required", never "predecessor ran earlier". Consumers pin exact versions.
 */

export type PhaseDeps = Record<string, string[]>;

/** Deterministic topological waves; declaration order breaks ties; cycles throw. */
export function buildWaves(phases: string[], deps: PhaseDeps): string[][] {
	const remaining = new Set(phases);
	const done = new Set<string>();
	const waves: string[][] = [];
	while (remaining.size > 0) {
		const wave = [...remaining].filter(name => (deps[name] ?? []).every(dep => done.has(dep))).sort();
		if (wave.length === 0) {
			throw new Error(`dependency cycle among phases: ${[...remaining].sort().join(", ")}`);
		}
		for (const name of wave) {
			remaining.delete(name);
			done.add(name);
		}
		waves.push(wave);
	}
	return waves;
}

export interface AcceptedVersion {
	phase: string;
	version: number;
	digest: string;
	artifacts: string[];
}

/**
 * Immutable version store: accept() appends; select() returns a frozen copy,
 * so a later producer correction cannot mutate an already-consumed input.
 */
export class VersionStore {
	private readonly versions = new Map<string, AcceptedVersion[]>();

	accept(record: AcceptedVersion): void {
		const history = this.versions.get(record.phase) ?? [];
		if (record.version !== history.length + 1) {
			throw new Error(`version gap for ${record.phase}: expected ${history.length + 1}`);
		}
		this.versions.set(record.phase, [...history, { ...record, artifacts: [...record.artifacts] }]);
	}

	select(phase: string, version?: number): AcceptedVersion | null {
		const history = this.versions.get(phase);
		if (!history || history.length === 0) return null;
		const found = version === undefined ? history.at(-1) : history.find(v => v.version === version);
		if (!found) return null;
		return { ...found, artifacts: [...found.artifacts] };
	}
}

/**
 * Ready = every required dependency has an accepted version AND this phase
 * has not already been decided (accepted or permanently failed).
 */
export function readyPhases(deps: PhaseDeps, accepted: ReadonlySet<string>, decided: ReadonlySet<string>): string[] {
	return Object.keys(deps)
		.filter(name => !decided.has(name))
		.filter(name => (deps[name] ?? []).every(dep => accepted.has(dep)))
		.sort();
}
