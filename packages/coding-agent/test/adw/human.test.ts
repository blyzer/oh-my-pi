/**
 * I11 against the shipped engine: a `human` phase is a gate, not a prompt.
 *
 * The engine already had the lane — `PhaseKind::Engineer`, "executed by the
 * caller and reported back" — and the ADW schema simply never exposed it.
 * What makes it a gate rather than a suggestion is that the phase cannot
 * pass without an answer, that a denial reaches the next agent as evidence,
 * and that a host with no way to ask refuses instead of assuming.
 */
import { describe, expect, it } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { TaskOutcomeKind, TaskPhaseKind, TaskRun } from "@oh-my-pi/pi-natives";
import { parseWorkflow } from "../../src/adw/config";

function workspace(): string {
	return fs.mkdtempSync(path.join(os.tmpdir(), "adw-human-"));
}

describe("the human lane", () => {
	it("is accepted by the workflow schema and reaches the engine as Engineer", () => {
		const workflow = parseWorkflow(
			"gate.yml",
			[
				"name: gate",
				"phases:",
				"  - name: publish",
				"    kind: human",
				"    description: Publish to the public registry?",
				"",
			].join("\n"),
		);
		const phase = workflow.phases.find(candidate => candidate.name === "publish");
		expect(phase?.kind).toBe("human");
		// The description is the question the operator is asked, so dropping
		// it would leave a gate nobody can answer meaningfully.
		expect(phase?.description).toBe("Publish to the public registry?");
	});

	it("holds the phase until answered, and a denial rejects it", () => {
		const root = workspace();
		const task = new TaskRun({
			adwId: `human-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "gate",
			root,
			maxAttempts: 1,
			phases: [{ name: "publish", kind: TaskPhaseKind.Engineer, owner: "operator" }],
		});
		task.nextStep();
		// The caller reports the human's answer through the same door a code
		// phase uses: a denial is a phase failure carrying its reason.
		const denied = task.submitCodeResult("publish", false, "signing key unavailable");
		expect(denied.kind).not.toBe(TaskOutcomeKind.Advanced);
	});

	it("advances only on an explicit approval", () => {
		const root = workspace();
		const task = new TaskRun({
			adwId: `human-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "gate",
			root,
			maxAttempts: 1,
			phases: [{ name: "publish", kind: TaskPhaseKind.Engineer, owner: "operator" }],
		});
		task.nextStep();
		expect(task.submitCodeResult("publish", true, "approved by operator").kind).toBe(TaskOutcomeKind.Advanced);
	});

	it("costs no tokens: the engine never dispatches an agent for it", () => {
		// The distinction that makes Engineer the right lane rather than an
		// agent phase with a human-shaped prompt. A Run step for this phase
		// is handed to the caller, not to a model.
		const root = workspace();
		const task = new TaskRun({
			adwId: `human-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "gate",
			root,
			maxAttempts: 1,
			phases: [{ name: "publish", kind: TaskPhaseKind.Engineer, owner: "operator" }],
		});
		const step = task.nextStep();
		expect(step.phase?.kind).toBe(TaskPhaseKind.Engineer);
		expect(step.phase?.name).toBe("publish");
	});

	it("refuses when the workflow declares a human phase the schema would reject as agent-shaped", () => {
		// A human phase has no owner-agent and no command. Accepting one that
		// also declares a command would leave two answers to the same
		// question, and the run would take the machine's.
		expect(() =>
			parseWorkflow(
				"gate.yml",
				["name: gate", "phases:", "  - name: publish", "    kind: human", '    command: "true"', ""].join("\n"),
			),
		).toThrow();
	});
});
