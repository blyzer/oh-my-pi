/**
 * The registered FactoryBench scenarios.
 *
 * Each drives `TaskRun` — the engine `/adw` actually uses — and asserts a
 * property that must survive any future optimization. Written against
 * observable outcomes, never internal call sequences, so a rewrite that
 * preserves the safety property keeps passing and one that quietly drops it
 * does not.
 */
import * as fs from "node:fs/promises";
import path from "node:path";
import { TaskOutcomeKind, TaskPhaseKind, TaskRun, TaskStepKind, TaskWriteGuard } from "@oh-my-pi/pi-natives";
import { classifyFailure } from "../task/admission";
import { defineScenario, type ScenarioOutcome } from "./bench";

function ok(evidence: string): ScenarioOutcome {
	return { passed: true, evidence };
}

function no(evidence: string): ScenarioOutcome {
	return { passed: false, evidence };
}

const SUCCESS = (artifacts: string[]): string => JSON.stringify({ status: "success", summary: "done", artifacts });

export const FAIL_CLOSED_001 = defineScenario({
	id: "FB-FAILCLOSED-001",
	family: "FB-FAILCLOSED",
	canonical: true,
	asserts:
		"A phase whose gates require a file to both contain and not contain the same marker can " +
		"never be accepted, however the producer behaves.",
	run: async workdir => {
		const marker = "__FACTORY_FAIL_CLOSED_CONTRADICTION_0024__";
		// The producer is maximally cooperative every time: it writes the
		// marker, reports success, declares exactly what it touched. Nothing
		// about its behaviour is the reason this must fail.
		for (const content of [`prefix ${marker} suffix`, "nothing here", ""]) {
			const root = path.join(workdir, `tree-${content.length}`);
			await fs.mkdir(root, { recursive: true });
			await Bun.write(path.join(root, "marker.txt"), content);
			const task = new TaskRun({
				adwId: `fb-failclosed-${Bun.randomUUIDv7().slice(0, 8)}`,
				workflow: "fb-failclosed",
				root,
				maxAttempts: 1,
				phases: [{ name: "build", kind: TaskPhaseKind.Agent, owner: "task" }],
				gates: [
					{
						phase: "build",
						gates: [`file_contains:marker.txt:${marker}`, `file_not_contains:marker.txt:${marker}`],
					},
				],
			});
			task.nextStep();
			const outcome = task.submitAgentOutput("build", SUCCESS(["marker.txt"]));
			if (outcome.kind === TaskOutcomeKind.Advanced) {
				return no(`accepted with content ${JSON.stringify(content)}`);
			}
		}
		return ok("refused for present, absent and empty content");
	},
});

export const GATE_001 = defineScenario({
	id: "FB-GATE-001",
	family: "FB-GATE",
	asserts: "Declaring no artifacts does not clear an artifact gate: claiming nothing is a rejection, not a pass.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		await fs.mkdir(root, { recursive: true });
		const task = new TaskRun({
			adwId: `fb-gate-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "fb-gate",
			root,
			maxAttempts: 1,
			phases: [{ name: "build", kind: TaskPhaseKind.Agent, owner: "task" }],
			gates: [{ phase: "build", gates: ["artifacts_exist"] }],
		});
		task.nextStep();
		// An empty claim used to pass both artifact gates vacuously.
		const outcome = task.submitAgentOutput("build", SUCCESS([]));
		return outcome.kind === TaskOutcomeKind.Advanced
			? no("an envelope declaring no artifacts cleared artifacts_exist")
			: ok("empty claim refused");
	},
});

export const DAG_001 = defineScenario({
	id: "FB-DAG-001",
	family: "FB-DAG",
	asserts:
		"A dependent phase never dispatches when its producer failed — readiness is an accepted " +
		"version, not a completed run.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		await fs.mkdir(root, { recursive: true });
		const task = new TaskRun({
			adwId: `fb-dag-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "fb-dag",
			root,
			maxAttempts: 1,
			phases: [
				{ name: "producer", kind: TaskPhaseKind.Agent, owner: "task", dependsOn: [] },
				{ name: "consumer", kind: TaskPhaseKind.Agent, owner: "task", dependsOn: ["producer"] },
			],
		});

		const dispatched: string[] = [];
		for (let guard = 0; guard < 8; guard++) {
			const step = task.nextStep();
			if (step.kind !== TaskStepKind.Run || !step.phase) break;
			const name = step.phase.name;
			dispatched.push(name);
			if (name === "producer") task.submitCodeResult(name, false, "producer failed");
			else task.submitAgentOutput(name, SUCCESS([]));
		}

		if (dispatched.includes("consumer")) return no(`consumer dispatched after a failed producer: ${dispatched}`);
		return ok(`dispatched ${dispatched.join(", ")}; consumer never ran`);
	},
});

export const PROVENANCE_001 = defineScenario({
	id: "FB-PROVENANCE-001",
	family: "FB-PROVENANCE",
	asserts: "A dispatched consumer is handed the exact producer version it consumes.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		await fs.mkdir(root, { recursive: true });
		await Bun.write(path.join(root, "plan.md"), "# plan\n");
		const task = new TaskRun({
			adwId: `fb-prov-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "fb-prov",
			root,
			maxAttempts: 1,
			phases: [
				{ name: "plan", kind: TaskPhaseKind.Agent, owner: "task", dependsOn: [] },
				{ name: "build", kind: TaskPhaseKind.Agent, owner: "task", dependsOn: ["plan"], inputs: ["plan"] },
			],
		});

		task.nextStep();
		task.submitAgentOutput("plan", SUCCESS(["plan.md"]));
		const step = task.nextStep();
		if (step.kind !== TaskStepKind.Run || step.phase?.name !== "build") {
			return no(`expected build to dispatch, got ${step.kind}`);
		}
		const consumed = step.inputs?.find(input => input.phase === "plan");
		if (!consumed) return no("build was dispatched with no recorded input");
		if (consumed.version !== 1) return no(`consumed version ${consumed.version}, expected 1`);
		return ok(`build consumed plan v${consumed.version}`);
	},
});

export const HUMAN_001 = defineScenario({
	id: "FB-HUMAN-001",
	family: "FB-HUMAN",
	asserts: "A human phase advances only on an explicit approval; a denial rejects it and carries the reason.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		await fs.mkdir(root, { recursive: true });
		const build = (): TaskRun =>
			new TaskRun({
				adwId: `fb-human-${Bun.randomUUIDv7().slice(0, 8)}`,
				workflow: "fb-human",
				root,
				maxAttempts: 1,
				phases: [{ name: "publish", kind: TaskPhaseKind.Engineer, owner: "operator" }],
			});

		const denied = build();
		denied.nextStep();
		if (denied.submitCodeResult("publish", false, "signing key unavailable").kind === TaskOutcomeKind.Advanced) {
			return no("a denied human phase advanced");
		}

		const approved = build();
		const step = approved.nextStep();
		// The lane matters: an Engineer phase is handed to the caller, never
		// dispatched to a model, so a human gate costs no tokens.
		if (step.phase?.kind !== TaskPhaseKind.Engineer) return no("human phase did not dispatch on the Engineer lane");
		if (approved.submitCodeResult("publish", true, "approved").kind !== TaskOutcomeKind.Advanced) {
			return no("an approved human phase did not advance");
		}
		return ok("denial rejected, approval advanced, dispatched as Engineer");
	},
});

export const CORRECTION_001 = defineScenario({
	id: "FB-CORRECTION-001",
	family: "FB-CORRECTION",
	asserts: "A failed gate returns the violation to the same phase as a correction, within the declared budget.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		await fs.mkdir(root, { recursive: true });
		const task = new TaskRun({
			adwId: `fb-corr-${Bun.randomUUIDv7().slice(0, 8)}`,
			workflow: "fb-corr",
			root,
			maxAttempts: 2,
			phases: [{ name: "build", kind: TaskPhaseKind.Agent, owner: "task" }],
			gates: [{ phase: "build", gates: ["artifacts_exist"] }],
		});

		task.nextStep();
		// Claims a file that is not there: the gate must name it back.
		const first = task.submitAgentOutput("build", SUCCESS(["missing.txt"]));
		if (first.kind !== TaskOutcomeKind.Retry) return no(`expected a retry, got outcome kind ${first.kind}`);
		if (!first.correction?.includes("missing.txt")) {
			return no(`correction did not name the missing artifact: ${first.correction ?? "none"}`);
		}

		// The second attempt satisfies it, and the budget was enough.
		await Bun.write(path.join(root, "missing.txt"), "now here\n");
		task.nextStep();
		const second = task.submitAgentOutput("build", SUCCESS(["missing.txt"]));
		if (second.kind !== TaskOutcomeKind.Advanced) return no("a satisfied retry did not advance");
		return ok(`corrected within budget: ${first.correction.slice(0, 60)}`);
	},
});

export const RESOURCE_001 = defineScenario({
	id: "FB-RESOURCE-001",
	family: "FB-RESOURCE",
	asserts:
		"A host failure is attributed to the host, not to the work — so a correction is never spent " +
		"asking a seat to fix code that never ran.",
	run: async () => {
		// The taxonomy itself, exercised at the boundary the runner uses. A
		// misread resource failure wastes an attempt; a misread semantic one
		// would skip a correction that was owed, so it fails toward semantic.
		const cases: Array<[string, "semantic" | "resource"]> = [
			["fatal error: out of memory", "resource"],
			["ENOSPC: no space left on device", "resource"],
			["killed by signal 9", "resource"],
			["expected 3 to equal 4", "semantic"],
			["TypeError: x is not a function", "semantic"],
		];
		for (const [text, expected] of cases) {
			const actual = classifyFailure(text);
			if (actual !== expected) return no(`classified ${JSON.stringify(text)} as ${actual}, expected ${expected}`);
		}
		// The provider layer owns its own errors: a 429 must not be re-derived
		// from message text here and drift from the taxonomy that already
		// knows it.
		if (classifyFailure("request failed", { status: 429 }) !== "resource") {
			return no("a provider rate limit was not attributed to the host");
		}
		if (classifyFailure("request failed", { status: 400 }) !== "semantic") {
			return no("a bad request was attributed to the host");
		}
		return ok(`${cases.length} signatures plus provider status classified correctly`);
	},
});

export const RECOVERY_001 = defineScenario({
	id: "FB-RECOVERY-001",
	family: "FB-RECOVERY",
	asserts:
		"An integration interrupted mid-apply is restored to its pre-apply state rather than left " +
		"half-landed, and the recovered tree is byte-exact.",
	run: async workdir => {
		const root = path.join(workdir, "tree");
		const runDir = path.join(workdir, "run");
		await fs.mkdir(root, { recursive: true });
		await fs.mkdir(runDir, { recursive: true });

		// Enough files that a partial apply is a real state rather than a
		// theoretical one: a single-file landing can never straddle a crash.
		const originals = new Map<string, string>();
		for (let index = 0; index < 200; index++) {
			const name = `file-${index}.txt`;
			const content = `original ${index}\n`;
			originals.set(name, content);
			await Bun.write(path.join(root, name), content);
		}

		const guard = TaskWriteGuard.create({ root, baselineFile: path.join(runDir, "recovery.json") });
		guard.begin();

		// A half-finished apply: some files rewritten, one created, the rest
		// untouched — what a SIGKILL mid-loop leaves behind.
		for (let index = 0; index < 80; index++) {
			await Bun.write(path.join(root, `file-${index}.txt`), `landed ${index}\n`);
		}
		await Bun.write(path.join(root, "appeared.txt"), "created mid-apply\n");

		// Recovery denies everything: the interrupted attempt authorized
		// nothing, so every trace of it must go.
		const restored = guard.settle({ allowed: [], protectedGlobs: [], patchDir: runDir });
		if (restored.unrecoverable.length > 0) {
			return no(`recovery could not restore: ${restored.unrecoverable.join(", ")}`);
		}

		for (const [name, content] of originals) {
			const actual = await Bun.file(path.join(root, name)).text();
			if (actual !== content) return no(`${name} recovered to ${JSON.stringify(actual)}, expected the original`);
		}
		if (await Bun.file(path.join(root, "appeared.txt")).exists()) {
			return no("a file created by the interrupted apply survived recovery");
		}
		return ok(`${originals.size} files byte-exact, mid-apply creation removed`);
	},
});
