/**
 * FactoryBench: a named, declarative scenario registry.
 *
 * The suite already covered these behaviours before this existed — as
 * assertions scattered across eighteen test files, where nobody could ask
 * "which safety family is unproven?" and get an answer. A test that exists
 * but cannot be named is coverage nobody can audit.
 *
 * This is deliberately NOT a second test runner. A scenario declares what it
 * exercises and returns a verdict; `bench.test.ts` runs the registry through
 * Bun like everything else. Building a parallel runtime for the benchmark of
 * a system whose whole point is not building parallel runtimes would be a
 * poor joke.
 */

/** The safety families a Factory must keep proven. */
export type BenchFamily =
	| "FB-DAG"
	| "FB-SCOPE"
	| "FB-GATE"
	| "FB-CORRECTION"
	| "FB-ISOLATION"
	| "FB-RECOVERY"
	| "FB-INTEGRATION"
	| "FB-HANDOFF"
	| "FB-HUMAN"
	| "FB-PROVENANCE"
	| "FB-FAILCLOSED"
	| "FB-RESOURCE";

export interface ScenarioOutcome {
	passed: boolean;
	/** What was observed, in the scenario's own terms. Shown on failure. */
	evidence: string;
}

export interface Scenario {
	/** Stable identifier, e.g. `FB-FAILCLOSED-001`. Referenced in reports. */
	id: string;
	family: BenchFamily;
	/** The property under test, phrased as what must remain true. */
	asserts: string;
	/**
	 * A scenario the Factory must never be able to optimize itself into
	 * passing incorrectly: it is held out of any tuning loop.
	 */
	canonical?: boolean;
	run: (workdir: string) => Promise<ScenarioOutcome>;
}

const registry = new Map<string, Scenario>();

/** Register a scenario. Duplicate ids are a wiring bug, not a merge nicety. */
export function defineScenario(scenario: Scenario): Scenario {
	if (registry.has(scenario.id)) throw new Error(`duplicate scenario id: ${scenario.id}`);
	registry.set(scenario.id, scenario);
	return scenario;
}

export function scenarios(): Scenario[] {
	return [...registry.values()].sort((a, b) => a.id.localeCompare(b.id));
}

export const BENCH_FAMILIES: BenchFamily[] = [
	"FB-DAG",
	"FB-SCOPE",
	"FB-GATE",
	"FB-CORRECTION",
	"FB-ISOLATION",
	"FB-RECOVERY",
	"FB-INTEGRATION",
	"FB-HANDOFF",
	"FB-HUMAN",
	"FB-PROVENANCE",
	"FB-FAILCLOSED",
	"FB-RESOURCE",
];

/**
 * Families with no registered scenario.
 *
 * The honest output of a benchmark is what it does NOT cover. A suite that
 * reports only its passes describes its author's attention, not the
 * system's safety.
 */
export function uncoveredFamilies(): BenchFamily[] {
	const covered = new Set(scenarios().map(scenario => scenario.family));
	return BENCH_FAMILIES.filter(family => !covered.has(family));
}
