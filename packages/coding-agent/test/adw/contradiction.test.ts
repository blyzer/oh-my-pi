/**
 * WI-0024 against the SHIPPED engine.
 *
 * The contradictory pair had to be expressible before it could be refusable:
 * the native gates covered artifact existence and JSON shape, so a workflow
 * could not state a requirement about a file's content at all, and the
 * canonical fail-closed case lived only in a parallel TypeScript stack.
 *
 * These drive `TaskRun` — the engine `/adw` actually uses.
 */
import { describe, expect, it } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { taskGateNames, TaskOutcomeKind, TaskPhaseKind, TaskRun } from "@oh-my-pi/pi-natives";

const MARKER = "__FACTORY_FAIL_CLOSED_CONTRADICTION_0024__";

function workspace(): string {
	return fs.mkdtempSync(path.join(os.tmpdir(), "adw-contradiction-"));
}

function build(root: string, gates: string[]): TaskRun {
	return new TaskRun({
		adwId: `wi0024-${Bun.randomUUIDv7().slice(0, 8)}`,
		workflow: "wi0024",
		root,
		maxAttempts: 1,
		phases: [{ name: "build", kind: TaskPhaseKind.Agent, owner: "task" }],
		gates: [{ phase: "build", gates }],
	});
}

/**
 * The most cooperative producer possible: succeeds, declares honestly, and
 * touches nothing it did not claim. Nothing about its behaviour is the
 * reason the contradiction must fail.
 */
function submitSuccess(task: TaskRun): TaskOutcomeKind {
	task.nextStep();
	return task.submitAgentOutput(
		"build",
		JSON.stringify({ status: "success", summary: "wrote it", artifacts: ["marker.txt"] }),
	).kind;
}

describe("WI-0024 against the shipped engine", () => {
	it("names the content gates so a workflow can request them", () => {
		const names = taskGateNames();
		expect(names).toContain("file_contains");
		expect(names).toContain("file_not_contains");
	});

	it("refuses a contradictory pair whatever the producer writes", () => {
		// Both directions plus the empty file: the property is that NO
		// content satisfies both, not that one particular content fails.
		for (const content of [`prefix ${MARKER} suffix`, "nothing here", ""]) {
			const root = workspace();
			fs.writeFileSync(path.join(root, "marker.txt"), content);
			const task = build(root, [`file_contains:marker.txt:${MARKER}`, `file_not_contains:marker.txt:${MARKER}`]);
			const kind = submitSuccess(task);
			expect(kind, `accepted with content ${JSON.stringify(content)}`).not.toBe(TaskOutcomeKind.Advanced);
		}
	});

	it("accepts a satisfiable content assertion, so the refusal is not vacuous", () => {
		// Without this the previous test would pass even if the gate always
		// failed — a gate that never passes is not a gate.
		const root = workspace();
		fs.writeFileSync(path.join(root, "marker.txt"), `prefix ${MARKER} suffix`);
		const task = build(root, [`file_contains:marker.txt:${MARKER}`]);
		expect(submitSuccess(task)).toBe(TaskOutcomeKind.Advanced);
	});

	it("fails a content assertion the tree does not satisfy", () => {
		const root = workspace();
		fs.writeFileSync(path.join(root, "marker.txt"), "nothing here");
		const task = build(root, [`file_contains:marker.txt:${MARKER}`]);
		expect(submitSuccess(task)).not.toBe(TaskOutcomeKind.Advanced);
	});

	it("refuses a malformed gate at construction rather than passing it silently", () => {
		const root = workspace();
		expect(() => build(root, ["file_contains:marker.txt"])).toThrow(/<path>:<marker>/);
		expect(() => build(root, ["file_contains::marker"])).toThrow(/non-empty/);
	});
});
