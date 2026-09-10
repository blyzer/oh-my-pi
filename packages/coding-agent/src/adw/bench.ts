/**
 * FactoryBench: a named, declarative scenario registry for the workflow
 * engine's safety properties.
 *
 * The suite already covered most of these behaviours — as assertions
 * scattered across test files, where nobody could ask "which safety family
 * is unproven?" and get an answer. A test that exists but cannot be named is
 * coverage nobody can audit.
 *
 * Deliberately NOT a second test runner. A scenario declares what it
 * exercises and returns a verdict; `test/adw/bench.test.ts` runs the
 * registry through Bun like everything else. Building a parallel runtime to
 * benchmark a system whose whole point is not building parallel runtimes
 * would be a poor joke.
 */

/** The safety families a workflow engine must keep proven. */
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

export const BENCH_FAMILIES: readonly BenchFamily[] = [
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
	 * A scenario the engine must never be able to optimize itself into
	 * passing incorrectly: held out of any tuning loop.
	 */
	canonical?: boolean;
	run: (workdir: string) => Promise<ScenarioOutcome>;
}

const registry = new Map<string, Scenario>();

/** Register a scenario. A duplicate id is a wiring bug, not a merge nicety. */
export function defineScenario(scenario: Scenario): Scenario {
	if (registry.has(scenario.id)) throw new Error(`duplicate scenario id: ${scenario.id}`);
	registry.set(scenario.id, scenario);
	return scenario;
}

export function scenarios(): Scenario[] {
	return [...registry.values()].sort((a, b) => a.id.localeCompare(b.id));
}

/**
 * Families with no registered scenario.
 *
 * The honest output of a benchmark is what it does NOT cover. A suite that
 * reports only its passes describes its author's attention, not the system's
 * safety.
 */
export function uncoveredFamilies(): BenchFamily[] {
	const covered = new Set(scenarios().map(scenario => scenario.family));
	return BENCH_FAMILIES.filter(family => !covered.has(family));
}
