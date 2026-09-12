import { describe, expect, it } from "bun:test";
import { ClassifierUnavailableError, classifyWorkflow, interpretChoice } from "../../src/adw/classify";
import type { AdwHost } from "../../src/adw/runner";
import type { DiscoveredWorkflow } from "../../src/adw/types";

const host = { cwd: "/tmp" } as AdwHost;

function workflow(name: string, description?: string): DiscoveredWorkflow {
	return {
		workflow: { name, description, phases: [{ name: "p", kind: "agent", owner: "task" }] },
		path: `/tmp/${name}.yml`,
		level: "project",
	} as DiscoveredWorkflow;
}

const catalogue = [workflow("fix", "repair a defect"), workflow("ship", "gate and deliver")];

describe("interpretChoice", () => {
	it("takes a name that is in the catalogue", () => {
		const choice = interpretChoice(JSON.stringify({ workflow: "ship", reason: "delivery" }), catalogue);
		expect(choice.workflow?.workflow.name).toBe("ship");
		expect(choice.reason).toBe("delivery");
	});

	it("refuses a name that is not", () => {
		// A workflow the model invented is not a near miss to repair: running
		// the closest-looking one would be a guess about the request.
		const choice = interpretChoice(JSON.stringify({ workflow: "deploy", reason: "sounds right" }), catalogue);
		expect(choice.workflow).toBeNull();
		expect(choice.reason).toContain("deploy");
	});

	it("carries an explicit refusal through with its reason", () => {
		// "Nothing fits" is an answer the operator can act on, so the model's
		// wording survives rather than being replaced by a generic message.
		const choice = interpretChoice(
			JSON.stringify({ workflow: null, reason: "needs a migration workflow" }),
			catalogue,
		);
		expect(choice.workflow).toBeNull();
		expect(choice.reason).toBe("needs a migration workflow");
	});

	it("reads an answer that opens with prose", () => {
		// Models asked for JSON still say "Sure --" often enough that demanding
		// a bare object would reject otherwise-correct answers.
		const choice = interpretChoice('Sure! {"workflow": "fix", "reason": "a defect"}', catalogue);
		expect(choice.workflow?.workflow.name).toBe("fix");
	});

	it("refuses output with no object in it at all", () => {
		const choice = interpretChoice("I think you should use ship.", catalogue);
		expect(choice.workflow).toBeNull();
	});
});

describe("classifyWorkflow", () => {
	it("does not ask when there is nothing to choose between", async () => {
		// One candidate is not a choice: the call would spend a model round
		// trip to learn nothing, and a "no" would be wrong.
		let asked = false;
		const choice = await classifyWorkflow("anything", [workflow("only")], {
			host,
			ask: async () => {
				asked = true;
				return { output: "{}", exitCode: 0 };
			},
		});
		expect(asked).toBeFalse();
		expect(choice.workflow?.workflow.name).toBe("only");
	});

	it("shows the model each workflow's phases and gates", async () => {
		// Routing is a judgement about the shape of the work -- whether a
		// reproducer is required, whether a review gates acceptance -- and a
		// bare list of names hides all of it.
		const withGates: DiscoveredWorkflow = {
			workflow: {
				name: "tdd",
				phases: [
					{ name: "red", kind: "code", owner: "sh", command: "t", gates: ["artifacts_exist"] },
					{ name: "fix", kind: "agent", owner: "task" },
				],
			},
			path: "/tmp/tdd.yml",
			level: "project",
		} as DiscoveredWorkflow;
		let prompt = "";
		await classifyWorkflow("req", [withGates, workflow("ship")], {
			host,
			ask: async task => {
				prompt = task;
				return { output: JSON.stringify({ workflow: "tdd", reason: "r" }), exitCode: 0 };
			},
		});
		expect(prompt).toContain("red (code)");
		expect(prompt).toContain("artifacts_exist");
	});

	it("distinguishes a classifier that could not run from one that declined", async () => {
		// A rate-limited provider has not decided anything. Reporting it as
		// "no workflow fits" sends the operator off to write one, when the
		// catalogue was never consulted.
		const failing = classifyWorkflow("req", catalogue, {
			host,
			ask: async () => ({ output: "", exitCode: 1, stderr: "429 quota exhausted" }),
		});
		await expect(failing).rejects.toBeInstanceOf(ClassifierUnavailableError);
	});

	it("treats an empty answer as unavailable, not as a refusal", async () => {
		// Exit 0 with no output is a provider that returned nothing, which is
		// the same absence of a decision as a crash.
		const empty = classifyWorkflow("req", catalogue, { host, ask: async () => ({ output: "   ", exitCode: 0 }) });
		await expect(empty).rejects.toBeInstanceOf(ClassifierUnavailableError);
	});

	it("reports an empty catalogue without asking", async () => {
		const choice = await classifyWorkflow("req", [], { host });
		expect(choice.workflow).toBeNull();
		expect(choice.reason).toContain("no workflows");
	});
});
