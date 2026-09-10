/**
 * The loader is judged against the frozen ADW examples themselves: they are
 * read out of the `adw-prototype-reference` tag, not copied into a fixture,
 * so a drift in either side shows up here.
 */
import { describe, expect, it } from "bun:test";
import { buildWaves } from "./dag";
import { loadWorkflowConfig } from "./workflow-config";

async function frozenExample(name: string): Promise<string> {
	const proc = Bun.spawn(["git", "show", `adw-prototype-reference:docs/adw/examples/${name}.yml`], {
		cwd: new URL("../../..", import.meta.url).pathname,
		stdout: "pipe",
		stderr: "pipe",
	});
	const [text, stderr, code] = await Promise.all([
		new Response(proc.stdout).text(),
		new Response(proc.stderr).text(),
		proc.exited,
	]);
	if ((code ?? 0) !== 0) throw new Error(stderr.trim());
	return text;
}

const options = {
	runAgent: async () => ({ changedFiles: [], label: "agent", exitCode: 0 }),
};

function waveNames(phases: Array<{ name: string; dependsOn?: string[] }>): string[][] {
	const deps = Object.fromEntries(phases.map(phase => [phase.name, [...(phase.dependsOn ?? [])]]));
	return buildWaves(Object.keys(deps), deps);
}

describe("loadWorkflowConfig against the frozen examples", () => {
	it("schedules fix.yml as build then verify, correcting back to build", async () => {
		const workflow = loadWorkflowConfig(await frozenExample("fix"), options);
		expect(waveNames(workflow.phases)).toEqual([["build"], ["verify"]]);
		const verify = workflow.phases.find(phase => phase.name === "verify");
		// `onFail: correct` must reach the phase that can change the result.
		expect(verify?.onReject).toEqual({ to: "build", maxRevisions: 3 });
		expect(verify?.gateCommand?.[2]).toBe("bun test");
		// Re-running an unchanged command spends the budget without changing
		// its input: the failure belongs to the corrector, not to a retry.
		expect(verify?.maxAttempts).toBe(1);
		const build = workflow.phases.find(phase => phase.name === "build");
		expect(build?.requireArtifacts).toBeTrue();
		expect(build?.artifactChecks).toEqual({ exist: true, nonEmpty: true, jsonParses: false });
	});

	it("carries ship.yml isolation, declared inputs and undeclared-write gating", async () => {
		const workflow = loadWorkflowConfig(await frozenExample("ship"), options);
		expect(workflow.isolation).toBeTrue();
		expect(waveNames(workflow.phases)).toEqual([["plan"], ["build"], ["verify"]]);
		expect(workflow.phases.find(phase => phase.name === "build")?.inputs).toEqual(["plan"]);
	});

	it("carries sdlc.yml write scopes, protected paths and the review route", async () => {
		const workflow = loadWorkflowConfig(await frozenExample("sdlc"), options);
		expect(workflow.acceptance).toBe("review");
		expect(workflow.protectedGlobs).toEqual([".omp/adw/**", "package.json"]);
		expect(waveNames(workflow.phases)).toEqual([["plan"], ["build"], ["docs"], ["verify"], ["review"]]);
		const build = workflow.phases.find(phase => phase.name === "build");
		expect(build?.scope).toEqual(["src/**", "test/**"]);
		expect(build?.writes).toBeTrue();
		const review = workflow.phases.find(phase => phase.name === "review");
		// `writes: []` is a read-only seat, not an unscoped one.
		expect(review?.scope).toEqual([]);
		expect(review?.writes).toBeFalse();
		expect(review?.onReject).toEqual({ to: "build", maxRevisions: 2 });
		// The prototype walks dependsOn breadth-first and takes the first agent,
		// so `verify: dependsOn [build, docs]` corrects `build`, not `docs`.
		expect(workflow.phases.find(phase => phase.name === "verify")?.onReject).toEqual({
			to: "build",
			maxRevisions: 3,
		});
	});
	it("loads review.yml as one fusion phase with a sole writer", async () => {
		const workflow = loadWorkflowConfig(await frozenExample("review"), options);
		expect(workflow.acceptance).toBe("review");
		const read = workflow.phases.find(phase => phase.name === "read");
		expect(read?.writes).toBeTrue();
		expect(read?.requireArtifacts).toBeTrue();
	});

	it("refuses a fusion phase with fewer than two panel seats", () => {
		const text = [
			"name: w",
			"phases:",
			"  - name: read",
			"    kind: fusion",
			"    panel:",
			"      - owner: reviewer",
			"    fuser:",
			"      owner: task",
			"",
		].join("\n");
		expect(() => loadWorkflowConfig(text, options)).toThrow("at least two panel seats");
	});

	it("refuses a fusion phase with no fuser", () => {
		const text = [
			"name: w",
			"phases:",
			"  - name: read",
			"    kind: fusion",
			"    panel:",
			"      - owner: reviewer",
			"      - owner: scout",
			"",
		].join("\n");
		expect(() => loadWorkflowConfig(text, options)).toThrow("needs a fuser");
	});
});

describe("loadWorkflowConfig refusals", () => {
	it("rejects an unknown gate rather than ignoring it", () => {
		const text = `name: w\nphases:\n  - name: a\n    kind: agent\n    gates: [vibes_ok]\n`;
		expect(() => loadWorkflowConfig(text, options)).toThrow('unsupported gate "vibes_ok"');
	});

	it("rejects artifact gates on a code phase, naming the alternative", () => {
		const text = `name: w\nphases:\n  - name: a\n    kind: code\n    command: "true"\n    gates: [artifacts_exist]\n`;
		expect(() => loadWorkflowConfig(text, options)).toThrow("cannot satisfy artifact gates");
	});

	it("rejects an input that is not a dependency", () => {
		const text = `name: w\nphases:\n  - name: a\n    kind: agent\n  - name: b\n    kind: agent\n    dependsOn: []\n    inputs: [a]\n`;
		expect(() => loadWorkflowConfig(text, options)).toThrow('consumes "a" without depending on it');
	});

	it("rejects onFail: correct with no agent to correct", () => {
		const text = `name: w\nphases:\n  - name: a\n    kind: code\n    command: "true"\n    onFail: correct\n`;
		expect(() => loadWorkflowConfig(text, options)).toThrow("no agent dependency to correct");
	});
});
