/**
 * Runs the FactoryBench registry through the ordinary test runner.
 *
 * The registry's value is not that it runs scenarios -- Bun already does
 * that -- but that each one is named, addressable, and grouped by the safety
 * family it defends, so an uncovered family is visible instead of merely
 * absent.
 */
import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { BENCH_FAMILIES, scenarios, uncoveredFamilies } from "./bench";
import "./scenarios";

const workdirs: string[] = [];

afterEach(async () => {
	await Promise.all(workdirs.splice(0).map(dir => fs.rm(dir, { recursive: true, force: true })));
});

describe("FactoryBench", () => {
	for (const scenario of scenarios()) {
		it(`${scenario.id} — ${scenario.asserts}`, async () => {
			const workdir = await fs.mkdtemp(path.join(os.tmpdir(), `bench-${scenario.id.toLowerCase()}-`));
			workdirs.push(workdir);
			const outcome = await scenario.run(workdir);
			// The evidence is the failure message: a scenario that fails
			// should say what it saw, not just that it was false.
			expect(outcome.passed, `${scenario.id}: ${outcome.evidence}`).toBeTrue();
		});
	}

	it("reports which safety families are unproven", () => {
		const uncovered = uncoveredFamilies();
		// Not an assertion that everything is covered -- it is not. This
		// fails only if the registry stops being able to answer the question,
		// which is the one thing a benchmark must never lose.
		expect(BENCH_FAMILIES.length).toBeGreaterThan(0);
		expect(uncovered.every(family => BENCH_FAMILIES.includes(family))).toBeTrue();
		if (uncovered.length > 0) console.log(`FactoryBench uncovered families: ${uncovered.join(", ")}`);
	});

	it("keeps the canonical fail-closed scenario registered and held out", () => {
		// FB-FAILCLOSED-001 is the case the Factory must never optimize
		// itself into passing incorrectly. Losing its `canonical` mark would
		// make it an ordinary scenario, eligible for tuning like any other.
		const canonical = scenarios().filter(scenario => scenario.canonical);
		expect(canonical.map(scenario => scenario.id)).toContain("FB-FAILCLOSED-001");
	});
});
