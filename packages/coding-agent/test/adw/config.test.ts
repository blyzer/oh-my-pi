import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, describe, expect, it } from "bun:test";
import {
	AdwConfigError,
	discoverWorkflows,
	loadWorkflow,
	parseWorkflow,
	rewindTarget,
} from "@oh-my-pi/pi-coding-agent/adw/config";

const VALID = `
name: ship
description: two phases
phases:
  - name: plan
    kind: agent
    owner: scout
    gates: [artifacts_exist]
  - name: test
    kind: code
    owner: bun
    command: bun test
`;

const roots: string[] = [];

function project(files: Record<string, string>): string {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "adw-config-"));
	roots.push(root);
	const dir = path.join(root, ".omp", "adw");
	fs.mkdirSync(dir, { recursive: true });
	for (const [name, body] of Object.entries(files)) fs.writeFileSync(path.join(dir, name), body);
	return root;
}

afterEach(() => {
	for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe("parseWorkflow", () => {
	it("accepts a workflow and keeps every phase in declared order", () => {
		const workflow = parseWorkflow("ship.yml", VALID);
		expect(workflow.name).toBe("ship");
		expect(workflow.phases.map(phase => phase.name)).toEqual(["plan", "test"]);
		expect(workflow.phases[0]?.gates).toEqual(["artifacts_exist"]);
	});

	it.each([
		["an agent phase with no owner", "kind: agent\n", "names no owner"],
		["a code phase with no command", "kind: code\n    owner: sh\n", "has no command"],
	])("rejects %s", (_label, phaseBody, expected) => {
		const yaml = `name: x\nphases:\n  - name: p\n    ${phaseBody}`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow(expected);
	});

	it("rejects a fusion phase with fewer than two seats — one seat is not a panel", () => {
		const yaml = `
name: x
phases:
  - name: p
    kind: fusion
    panel:
      - { owner: scout }
    fuser: { owner: task }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow("at least two panel seats");
	});

	it("rejects a fusion phase with no fuser — nobody would be allowed to write", () => {
		const yaml = `
name: x
phases:
  - name: p
    kind: fusion
    panel:
      - { owner: scout }
      - { owner: sonic }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow("names no fuser");
	});

	it("rejects panel/fuser on a non-fusion phase", () => {
		const yaml = `
name: x
phases:
  - name: p
    kind: agent
    owner: scout
    fuser: { owner: task }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow("only apply to fusion phases");
	});

	it("rejects duplicate phase names — the engine keys gates and trace records by name", () => {
		const yaml = `
name: x
phases:
  - { name: p, kind: code, owner: sh, command: "true" }
  - { name: p, kind: code, owner: sh, command: "true" }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow('duplicate phase name "p"');
	});

	it("rejects a gate the engine cannot build, and names the ones it can", () => {
		const yaml = `
name: x
phases:
  - { name: p, kind: agent, owner: scout, gates: [nope] }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow(/unknown gate "nope".*artifacts_exist/s);
	});

	it("rejects timeoutMs <= 0 — ptree attaches no deadline at all for that", () => {
		const yaml = `
name: x
phases:
  - { name: p, kind: code, owner: sh, command: "true", timeoutMs: 0 }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow("must be greater than zero");
	});

	it("rejects a fractional maxAttempts rather than silently rounding the budget", () => {
		const yaml = `
name: x
maxAttempts: 1.5
phases:
  - { name: p, kind: code, owner: sh, command: "true" }
`;
		expect(() => parseWorkflow("x.yml", yaml)).toThrow("whole number");
	});

	it("reports the file name on malformed YAML", () => {
		expect(() => parseWorkflow("broken.yml", "name: x\n  bad: [indent")).toThrow(/^broken\.yml:/);
	});

	it("rejects a misspelled key instead of ignoring it", () => {
		// `isolaton: true` parsing cleanly would run the workflow against the real
		// checkout while the operator believes it is sandboxed.
		expect(() =>
			parseWorkflow(
				"typo.yml",
				'name: t\nisolaton: true\nphases:\n  - { name: p, kind: code, owner: sh, command: "true" }',
			),
		).toThrow(/isolaton/);
	});

	it("rejects a misspelled key inside a phase", () => {
		const yaml = `name: t\nphases:\n  - { name: p, kind: code, owner: sh, command: "true", tiemoutMs: 5 }`;
		expect(() => parseWorkflow("typo.yml", yaml)).toThrow(/tiemoutMs/);
	});

	it("rejects a YAML document that is not a mapping", () => {
		expect(() => parseWorkflow("list.yml", "- one\n- two")).toThrow("expected a YAML mapping");
	});
});

describe("concurrency", () => {
	const workflow = (value: string) =>
		`name: x\nconcurrency: ${value}\nisolation: true\nphases:\n  - { name: p, kind: code, owner: sh, command: "true" }`;

	it.each([["0"], ["1.5"], ["9"]])("rejects %s outside the bounded integer dispatch width", value => {
		expect(() => parseWorkflow("x.yml", workflow(value))).toThrow(/concurrency must be a whole number from 1 to 8/);
	});

	it.each([[""], ["isolation: false\n"]])("rejects concurrent execution without isolation: %s", isolation => {
		expect(() => parseWorkflow("x.yml", workflow("2").replace("isolation: true\n", isolation))).toThrow(
			/concurrency greater than 1 requires isolation: true/,
		);
	});

	it.each([
		["agent", "kind: agent, owner: task"],
		["fusion", "kind: fusion, panel: [{ owner: scout }, { owner: reviewer }], fuser: { owner: task }"],
	])("requires a write declaration on a concurrent %s writer, allowing an explicit deny-write scope", (_kind, writer) => {
		const yaml = `name: w\nconcurrency: 8\nisolation: true\nphases:
  - { name: build, ${writer}, writes: [] }
  - { name: verify, kind: code, command: "bun check", onFail: correct }
`;
		expect(rewindTarget(parseWorkflow("w.yml", yaml), "verify")).toBe("build");
		expect(() => parseWorkflow("w.yml", yaml.replace(", writes: []", ""))).toThrow(/requires explicit writes/);
	});

	it("requires concurrent review acceptance to cover every contributing writer, directly or through checks", () => {
		const yaml = `name: w\nconcurrency: 2\nisolation: true\nacceptance: review\nphases:
  - { name: api, kind: agent, owner: task, writes: ["src/api/**"], dependsOn: [] }
  - { name: ui, kind: agent, owner: task, writes: ["src/ui/**"], dependsOn: [] }
  - { name: check, kind: code, command: "bun check", dependsOn: [api, ui] }
  - { name: review, kind: agent, owner: reviewer, writes: [], gates: [verdict_consistent], onReject: { to: api, maxRevisions: 1 } }
`;
		parseWorkflow("w.yml", yaml);
		expect(() => parseWorkflow("w.yml", yaml.replace("dependsOn: [api, ui]", "dependsOn: [api]"))).toThrow(
			/must depend on concurrent writer "ui"/,
		);
		expect(() => parseWorkflow("w.yml", yaml.replace(", gates: [verdict_consistent], onReject: { to: api, maxRevisions: 1 }", ""))).toThrow(
			/concurrent review acceptance requires final phase/,
		);
	});
});

describe("discoverWorkflows", () => {
	it("reports a broken file instead of dropping it, and still returns the good ones", async () => {
		const cwd = project({ "ship.yml": VALID, "review.yml": "name: review\nphases:\n  - name: p\n    kind: agent\n" });
		const { workflows, problems } = await discoverWorkflows(cwd);

		expect(workflows.get("ship")?.workflow.name).toBe("ship");
		// Filtered: discovery also walks the operator's own `~/.omp/agent/adw`,
		// and their broken files are not this test's business.
		const mine = problems.filter(problem => problem.includes("review.yml"));
		expect(mine).toHaveLength(1);
		expect(mine[0]).toContain("names no owner");
	});

	it("keys workflows by their declared name, not their file name", async () => {
		const cwd = project({ "anything.yml": VALID });
		const { workflows } = await discoverWorkflows(cwd);
		expect(workflows.has("ship")).toBe(true);
		expect(workflows.has("anything")).toBe(false);
	});

	it("keeps the first of two files claiming the same name", async () => {
		const cwd = project({
			"a-first.yml": VALID,
			"b-second.yml": VALID.replace("description: two phases", "description: the loser"),
		});
		const { workflows } = await discoverWorkflows(cwd);
		expect(workflows.get("ship")?.workflow.description).toBe("two phases");
	});
});

describe("loadWorkflow", () => {
	it("explains why a named-but-broken workflow did not load", async () => {
		const cwd = project({ "review.yml": "name: review\nphases:\n  - name: p\n    kind: agent\n" });
		// The operator asked for `review` by name. "Unknown workflow" alone would
		// be false — it exists, it failed to parse, and that error is the answer.
		const error = await loadWorkflow(cwd, "review").catch((err: unknown) => err);
		expect(error).toBeInstanceOf(AdwConfigError);
		expect((error as Error).message).toContain("review.yml");
		expect((error as Error).message).toContain("names no owner");
	});

	it("returns the workflow and where it came from", async () => {
		const cwd = project({ "ship.yml": VALID });
		const found = await loadWorkflow(cwd, "ship");
		expect(found.workflow.name).toBe("ship");
		expect(found.level).toBe("project");
		expect(found.path.endsWith("ship.yml")).toBe(true);
	});
});

describe("onFail: correct", () => {
	const yaml = (phases: string) => `name: w\nphases:\n${phases}`;

	it("rejects a code phase whose failure has nobody to correct", () => {
		// The engine would only discover this after something already failed —
		// the worst moment to learn the workflow was malformed.
		expect(() =>
			parseWorkflow("w.yml", yaml('  - { name: t, kind: code, owner: sh, command: "true", onFail: correct }')),
		).toThrow(/no agent or fusion phase precedes it/);
	});

	it("rejects onFail on a phase that is not a code phase", () => {
		expect(() => parseWorkflow("w.yml", yaml("  - { name: p, kind: agent, owner: sonic, onFail: correct }"))).toThrow(
			/only applies to code phases/,
		);
	});

	it("accepts a code phase preceded by an agent phase", () => {
		const workflow = parseWorkflow(
			"w.yml",
			yaml(
				'  - { name: build, kind: agent, owner: sonic }\n  - { name: t, kind: code, owner: sh, command: "true", onFail: correct }',
			),
		);
		expect(rewindTarget(workflow, "t")).toBe("build");
	});

	it("skips code phases when resolving the target", () => {
		// Correcting a code phase would mean re-running a command that already
		// decided; only a phase with an agent can change the outcome.
		const workflow = parseWorkflow(
			"w.yml",
			yaml(
				'  - { name: build, kind: agent, owner: sonic }\n  - { name: fmt, kind: code, owner: sh, command: "true" }\n  - { name: t, kind: code, owner: sh, command: "true", onFail: correct }',
			),
		);
		expect(rewindTarget(workflow, "t")).toBe("build");
	});

	it("resolves a fusion phase as a target", () => {
		const workflow = parseWorkflow(
			"w.yml",
			yaml(
				'  - name: design\n    kind: fusion\n    panel: [{ owner: scout }, { owner: reviewer }]\n    fuser: { owner: sonic }\n  - { name: t, kind: code, owner: sh, command: "true", onFail: correct }',
			),
		);
		expect(rewindTarget(workflow, "t")).toBe("design");
	});
});

/**
 * The examples are copied into real repos, so a schema change that invalidates
 * one has to fail here rather than in a user's first run. Reading the shipped
 * directory rather than a fixture is the point: a new example is covered the
 * moment it lands.
 */
describe("shipped example workflows", () => {
	const dir = path.join(import.meta.dir, "../../../../docs/adw/examples");
	const files = fs.readdirSync(dir).filter(name => name.endsWith(".yml"));

	it("ships at least one example", () => {
		// Zero examples was the state this test exists to prevent returning to.
		expect(files.length).toBeGreaterThan(0);
	});

	for (const file of files) {
		it(`parses and validates ${file}`, () => {
			const workflow = parseWorkflow(file, fs.readFileSync(path.join(dir, file), "utf8"));
			expect(workflow.name).toBe(file.replace(/\.yml$/, ""));
			expect(workflow.phases.length).toBeGreaterThan(0);
			// An example advertising `onFail: correct` must really have a
			// correctable predecessor, not pass on a technicality.
			for (const phase of workflow.phases) {
				if (phase.onFail === "correct") expect(rewindTarget(workflow, phase.name)).toBeTruthy();
			}
		});
	}
});

describe("dependsOn", () => {
	const yaml = (phases: string) => `name: w\nphases:\n${phases}`;
	const agent = (name: string, deps?: string) =>
		`  - { name: ${name}, kind: agent, owner: sonic${deps !== undefined ? `, dependsOn: [${deps}] }` : " }"}`;

	it("rejects a dependency on a phase that does not exist", () => {
		expect(() => parseWorkflow("w.yml", yaml(`${agent("api")}\n${agent("docs", "apii")}`))).toThrow(
			/depends on unknown phase "apii"/,
		);
	});

	it("rejects a phase that depends on itself", () => {
		expect(() => parseWorkflow("w.yml", yaml(agent("api", "api")))).toThrow(/depends on itself/);
	});

	it("stops correction traversal at explicit independence rather than rewinding an unrelated predecessor", () => {
		const yaml = `name: w\nphases:
  - { name: unrelated, kind: agent, owner: task }
  - { name: check, kind: code, command: "bun check", dependsOn: [] }
  - { name: verify, kind: code, command: "bun test", onFail: correct }
`;
		expect(() => parseWorkflow("w.yml", yaml)).toThrow(/no agent or fusion phase precedes it/);
		expect(rewindTarget(parseWorkflow("w.yml", yaml.replace(", dependsOn: []", "")), "verify")).toBe("unrelated");
	});

	it("rejects a cycle formed by an implicit predecessor edge", () => {
		expect(() => parseWorkflow("w.yml", yaml(`${agent("a", "b")}\n${agent("b")}`))).toThrow(/dependency cycle/);
	});

	it("walks implicit edges behind an explicit code dependency when finding a correction target", () => {
		const workflow = parseWorkflow("w.yml", yaml(
			`${agent("build")}\n  - { name: check, kind: code, command: "bun check" }\n` +
			`  - { name: verify, kind: code, command: "bun test", dependsOn: [check], onFail: correct }`,
		));
		expect(rewindTarget(workflow, "verify")).toBe("build");
	});

	it("rejects a cycle and names every phase in it", () => {
		// The engine leaves an unorderable graph in declaration order rather than
		// inventing one, so this has to be caught here or the workflow silently
		// runs in a sequence nobody wrote.
		const error = (() => {
			try {
				parseWorkflow("w.yml", yaml(`${agent("a", "c")}\n${agent("b", "a")}\n${agent("c", "b")}`));
			} catch (err) {
				return err instanceof Error ? err.message : String(err);
			}
			return "no error";
		})();
		expect(error).toContain("dependency cycle");
		for (const name of ["a", "b", "c"]) expect(error).toContain(name);
	});

	it("sends a failure back to a dependency, not to whatever was declared before", () => {
		// Position is the wrong answer once a graph exists: `unrelated` runs
		// earlier but has no business receiving another phase's failure.
		const workflow = parseWorkflow(
			"w.yml",
			yaml(
				`${agent("unrelated")}\n${agent("api")}\n  - { name: verify, kind: code, owner: sh, command: "true", onFail: correct, dependsOn: [api] }`,
			),
		);
		expect(rewindTarget(workflow, "verify")).toBe("api");
	});

	it("walks past a code dependency to the nearest phase that can act", () => {
		const workflow = parseWorkflow(
			"w.yml",
			yaml(
				`${agent("api")}\n  - { name: fmt, kind: code, owner: sh, command: "true", dependsOn: [api] }\n  - { name: verify, kind: code, owner: sh, command: "true", onFail: correct, dependsOn: [fmt] }`,
			),
		);
		expect(rewindTarget(workflow, "verify")).toBe("api");
	});
});

describe("inputs", () => {
	const yaml = (phases: string) => `name: w\nphases:\n${phases}`;
	const agent = (name: string, extra = "") => `  - { name: ${name}, kind: agent, owner: sonic${extra} }`;

	it("rejects an input naming a phase that does not exist", () => {
		expect(() => parseWorkflow("w.yml", yaml(`${agent("plan")}\n${agent("build", ", inputs: [plaan]")}`))).toThrow(
			/phase "build" consumes unknown input "plaan"/,
		);
	});

	it("rejects a phase that consumes its own output", () => {
		expect(() => parseWorkflow("w.yml", yaml(agent("plan", ", inputs: [plan]")))).toThrow(
			/phase "plan" lists itself as an input/,
		);
	});

	it("rejects a duplicated input rather than delivering one envelope twice", () => {
		expect(() =>
			parseWorkflow("w.yml", yaml(`${agent("plan")}\n${agent("build", ", inputs: [plan, plan]")}`)),
		).toThrow(/phase "build" lists input "plan" more than once/);
	});

	it("rejects an input that executes later — its output cannot exist yet", () => {
		expect(() => parseWorkflow("w.yml", yaml(`${agent("build", ", inputs: [plan]")}\n${agent("plan")}`))).toThrow(
			/phase "build" input "plan" is not a transitive dependency/,
		);
	});

	it("rejects an input outside the declared dependency closure", () => {
		// With a graph declared, data flow and ordering must agree: consuming a
		// phase this one does not depend on races the scheduler.
		expect(() =>
			parseWorkflow(
				"w.yml",
				yaml(`${agent("plan")}\n${agent("other", ", dependsOn: []")}\n${agent("build", ", dependsOn: [other], inputs: [plan]")}`),
			),
		).toThrow(/phase "build" input "plan" is not a transitive dependency/);
	});

	it("requires data flow to follow implicit edges, not merely declaration order", () => {
		const workflow = yaml(
			`${agent("plan")}\n${agent("other", ", dependsOn: []")}\n${agent("build", ", inputs: [plan]")}`,
		);
		expect(() => parseWorkflow("w.yml", workflow)).toThrow(/not a transitive dependency/);
	});

	it("resolves later-declared inputs through transitive graph ordering", () => {
		const workflow = yaml(
			`  - { name: verify, kind: code, command: "bun test", dependsOn: [mid], inputs: [plan], onFail: correct }\n` +
			`  - { name: mid, kind: code, command: "bun check", dependsOn: [plan] }\n` +
			agent("plan", ", dependsOn: []"),
		);
		expect(rewindTarget(parseWorkflow("w.yml", workflow), "verify")).toBe("plan");
	});
});

describe("artifact gates on a code phase", () => {
	const yaml = (gate: string) =>
		`name: w\nphases:\n  - { name: t, kind: code, owner: sh, command: "true", gates: [${gate}] }`;

	for (const gate of ["artifacts_exist", "files_non_empty"]) {
		it(`rejects ${gate} on a code phase instead of failing it at run time`, () => {
			// A code phase declares no artifacts, and an empty claim fails these
			// gates rather than passing vacuously — so this combination can only
			// ever fail. Catching it at load costs nothing; catching it mid-run
			// costs a phase.
			expect(() => parseWorkflow("w.yml", yaml(gate))).toThrow(/cannot satisfy gate/);
		});
	}

	it("still allows diff_matches_claims, which examines the tree rather than a claim", () => {
		expect(() => parseWorkflow("w.yml", yaml("diff_matches_claims"))).not.toThrow();
	});
});

describe("write scopes", () => {
	it("rejects writes on a code phase, naming the phase — a command has no guarded writer", () => {
		const yaml = `name: w\nphases:\n  - { name: t, kind: code, owner: sh, command: "true", writes: ["src/**"] }`;
		expect(() => parseWorkflow("w.yml", yaml)).toThrow(
			'phase "t" is a code phase; writes only applies to agent and fusion phases',
		);
	});

	it("rejects a blank writes glob — natively it would compile and silently authorize nothing", () => {
		const yaml = `name: w\nphases:\n  - { name: p, kind: agent, owner: scout, writes: ["src/**", "  "] }`;
		expect(() => parseWorkflow("w.yml", yaml)).toThrow('phase "p" writes contains an empty glob');
	});

	it("rejects a duplicate writes glob", () => {
		const yaml = `name: w\nphases:\n  - { name: p, kind: agent, owner: scout, writes: ["src/**", "src/**"] }`;
		expect(() => parseWorkflow("w.yml", yaml)).toThrow('phase "p" writes lists "src/**" more than once');
	});

	it("rejects an empty protected glob at the workflow level", () => {
		const yaml = `name: w\nprotected: [""]\nphases:\n  - { name: p, kind: agent, owner: scout }`;
		expect(() => parseWorkflow("w.yml", yaml)).toThrow("protected contains an empty glob");
	});
});

describe("review contracts", () => {
	it("rejects a review gate on a code phase that cannot emit the review contract", () => {
		expect(() =>
			parseWorkflow(
				"w.yml",
				'name: w\nphases:\n  - { name: test, kind: code, command: "true", gates: [verdict_consistent] }',
			),
		).toThrow(/verdict_consistent requires an agent or fusion review envelope/);
	});
});

describe("onReject", () => {
	const yaml = (route: string, source = "kind: agent, owner: reviewer, gates: [verdict_consistent]") =>
		`name: w\nphases:\n  - { name: build, kind: agent, owner: task }\n  - { name: review, ${source}, onReject: ${route} }`;

	it.each([
		["missing target", "{ maxRevisions: 1 }", /to/],
		["missing revision limit", "{ to: build }", /maxRevisions/],
		["misspelled route key", "{ to: build, maxRevisions: 1, maxRevisons: 2 }", /maxRevisons/],
		["non-string target", "{ to: 7, maxRevisions: 1 }", /to/],
		["fractional revision limit", "{ to: build, maxRevisions: 1.5 }", /whole number/],
		["zero revision limit", "{ to: build, maxRevisions: 0 }", /whole number/],
		["overflowing revision limit", "{ to: build, maxRevisions: 65536 }", /whole number/],
		["unknown target", "{ to: absent, maxRevisions: 1 }", /unknown phase "absent"/],
		["self target", "{ to: review, maxRevisions: 1 }", /targets itself/],
	] as const)("rejects %s before starting a run", (_label, route, error) => {
		expect(() => parseWorkflow("w.yml", yaml(route))).toThrow(error);
	});

	it("requires an explicit verdict gate rather than inferring review intent", () => {
		expect(() =>
			parseWorkflow("w.yml", yaml("{ to: build, maxRevisions: 1 }", "kind: agent, owner: reviewer")),
		).toThrow(/requires the verdict_consistent gate/);
	});

	it("rejects a code source even when a builder exists", () => {
		expect(() =>
			parseWorkflow("w.yml", yaml("{ to: build, maxRevisions: 1 }", 'kind: code, command: "true"')),
		).toThrow(/onReject requires an agent or fusion review envelope/);
	});

	it("rejects a code target because reexecuting checks cannot revise the implementation", () => {
		expect(() =>
			parseWorkflow(
				"w.yml",
				yaml("{ to: build, maxRevisions: 1 }").replace(
					"name: build, kind: agent, owner: task",
					'name: build, kind: code, command: "true"',
				),
			),
		).toThrow(/must be an agent or fusion phase/);
	});

	it("accepts a later-declared transitive builder only when it executes before the review", () => {
		const workflow = `name: w\nphases:
  - { name: review, kind: fusion, panel: [{ owner: scout }, { owner: reviewer }], fuser: { owner: task }, gates: [verdict_consistent], dependsOn: [checks], onReject: { to: build, maxRevisions: 65535 } }
  - { name: checks, kind: code, command: "true", dependsOn: [build] }
  - { name: build, kind: fusion, panel: [{ owner: scout }, { owner: reviewer }], fuser: { owner: task }, dependsOn: [] }
`;
		parseWorkflow("w.yml", workflow);
		expect(() => parseWorkflow("w.yml", workflow.replace("dependsOn: [checks], ", ""))).toThrow(
			/not a transitive dependency/,
		);
	});

	it("rejects an earlier-declared target whose dependencies delay it until after the review", () => {
		const workflow = yaml("{ to: build, maxRevisions: 1 }")
			.replace(
				"name: build, kind: agent, owner: task",
				"name: build, kind: agent, owner: task, dependsOn: [prepare]",
			)
			.replace("gates: [verdict_consistent]", "gates: [verdict_consistent], dependsOn: []")
			.concat("\n  - { name: prepare, kind: agent, owner: scout, dependsOn: [] }");
		expect(() => parseWorkflow("w.yml", workflow)).toThrow(/not a transitive dependency/);
	});

	it("does not route to an unrelated builder when dependencies are declared, even if empty", () => {
		for (const deps of ["[other]", "[]"]) {
			const workflow = yaml(
				"{ to: build, maxRevisions: 1 }",
				`kind: agent, owner: reviewer, gates: [verdict_consistent], dependsOn: ${deps}`,
			).replace("phases:", "phases:\n  - { name: other, kind: agent, owner: scout }");
			expect(() => parseWorkflow("w.yml", workflow)).toThrow(/not a transitive dependency/);
		}
	});

	it("does not revise an unrelated builder across an implicit predecessor edge", () => {
		const workflow = yaml("{ to: build, maxRevisions: 1 }").replace(
			"  - { name: review",
			"  - { name: separate, kind: code, command: \"bun check\", dependsOn: [] }\n  - { name: review",
		);
		expect(() => parseWorkflow("w.yml", workflow)).toThrow(/not a transitive dependency/);
	});

	it("rejects a dependency cycle rather than treating its builder as an earlier revision target", () => {
		const workflow = yaml(
			"{ to: build, maxRevisions: 1 }",
			"kind: agent, owner: reviewer, gates: [verdict_consistent], dependsOn: [build]",
		).replace("name: build, kind: agent, owner: task", "name: build, kind: agent, owner: task, dependsOn: [review]");
		expect(() => parseWorkflow("w.yml", workflow)).toThrow(/dependency cycle/);
	});
});

describe("payload schema", () => {
	const yaml = (schema: string) =>
		`name: w\nphases:\n  - name: p\n    kind: agent\n    owner: sonic\n    schema:\n${schema}`;

	it("rejects a conditional omptype would silently drop", () => {
		// Measured: fromJsonSchema compiles if/then and then validates
		// {approved: true, blocking: ["x"]} as fine. A conditional that does
		// nothing reads like a guarantee, which is worse than not having one.
		expect(() =>
			parseWorkflow(
				"w.yml",
				yaml("      type: object\n      if: { properties: { a: { const: true } } }\n      then: { required: [b] }"),
			),
		).toThrow(/does not enforce if, then/);
	});

	it("rejects a conditional nested deeper in the document", () => {
		expect(() =>
			parseWorkflow(
				"w.yml",
				yaml("      type: object\n      properties:\n        a: { oneOf: [{ type: string }] }"),
			),
		).toThrow(/does not enforce oneOf/);
	});

	it("accepts the structural keywords that actually run", () => {
		const workflow = parseWorkflow(
			"w.yml",
			yaml("      type: object\n      required: [approved]\n      properties:\n        approved: { type: boolean }"),
		);
		expect(workflow.phases[0]?.schema).toBeDefined();
	});

	it("rejects a schema on a code phase, which has no payload", () => {
		expect(() =>
			parseWorkflow(
				"w.yml",
				'name: w\nphases:\n  - { name: p, kind: code, owner: sh, command: "true", schema: { type: object } }',
			),
		).toThrow(/does not apply/);
	});
});
