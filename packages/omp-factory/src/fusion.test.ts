/**
 * Fusion behaviour: a read-only panel answers the same question in parallel,
 * one seat writes, and a retry re-polls nobody.
 */
import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { runGraph } from "./graph";
import type { Candidate } from "./loop";
import { type AgentRunContext, loadWorkflowConfig } from "./workflow-config";

let root = "";
let runDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-fusion-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-fusion-run-"));
}

afterEach(async () => {
	for (const dir of [root, runDir]) {
		if (dir) await fs.rm(dir, { recursive: true, force: true });
	}
	root = "";
	runDir = "";
});

const FUSION_YAML = [
	"name: review",
	"maxAttempts: 2",
	"phases:",
	"  - name: read",
	"    kind: fusion",
	"    prompt: Review the code.",
	"    panel:",
	"      - owner: reviewer",
	"      - owner: security-reviewer",
	"    fuser:",
	"      owner: task",
	"",
].join("\n");

describe("fusion phases", () => {
	it("runs the panel read-only and hands labelled opinions to the sole writer", async () => {
		await makeDirs();
		const calls: AgentRunContext[] = [];
		const workflow = loadWorkflowConfig(FUSION_YAML, {
			runAgent: async context => {
				calls.push(context);
				return {
					changedFiles: [],
					label: context.owner,
					exitCode: 0,
					output: `${context.owner} says so`,
				} satisfies Candidate;
			},
		});
		const result = await runGraph({
			allowUnguardedWrites: true,
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: workflow.phases,
		});

		expect(result.status).toBe("accepted");
		// Two panel seats plus the fuser.
		expect(calls).toHaveLength(3);
		expect(calls.slice(0, 2).map(call => call.owner)).toEqual(["reviewer", "security-reviewer"]);
		expect(calls.slice(0, 2).every(call => call.readOnly)).toBeTrue();
		const fuserCall = calls[2];
		expect(fuserCall?.owner).toBe("task");
		expect(fuserCall?.readOnly).toBeFalse();
		// Opinions reach the fuser labelled and in declared order, never merged early.
		expect(fuserCall?.opinions?.map(opinion => opinion.owner)).toEqual(["reviewer", "security-reviewer"]);
		expect(fuserCall?.opinions?.[0]?.text).toBe("reviewer says so");
	});

	it("pins each seat to its declared model and thinking level", async () => {
		await makeDirs();
		const seen: Array<{ owner: string; model?: string; thinking?: string }> = [];
		const pinned = [
			"name: review",
			"phases:",
			"  - name: read",
			"    kind: fusion",
			"    panel:",
			"      - owner: reviewer",
			"        model: anthropic/claude-opus-4-5",
			"      - owner: scout",
			"        model: openai/gpt-5.5",
			"    fuser:",
			"      owner: task",
			"      thinking: high",
			"",
		].join("\n");
		const workflow = loadWorkflowConfig(pinned, {
			runAgent: async context => {
				seen.push({ owner: context.owner, model: context.model, thinking: context.thinking });
				return { changedFiles: [], label: context.owner, exitCode: 0, output: "opinion" };
			},
		});
		await runGraph({
			allowUnguardedWrites: true,
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: workflow.phases,
		});

		// Distinct seats reach distinct models: a panel resolving to one model
		// would report two opinions while holding one.
		expect(seen).toEqual([
			{ owner: "reviewer", model: "anthropic/claude-opus-4-5", thinking: undefined },
			{ owner: "scout", model: "openai/gpt-5.5", thinking: undefined },
			{ owner: "task", model: undefined, thinking: "high" },
		]);
	});

	it("re-runs only the fuser on a retry", async () => {
		await makeDirs();
		const owners: string[] = [];
		let fuserRuns = 0;
		const workflow = loadWorkflowConfig(FUSION_YAML, {
			runAgent: async context => {
				owners.push(context.owner);
				if (context.owner !== "task") {
					return { changedFiles: [], label: context.owner, exitCode: 0, output: "opinion" };
				}
				fuserRuns += 1;
				return {
					changedFiles: [],
					label: "fuser",
					exitCode: 0,
					// First fuser attempt violates the contract; the second conforms.
					envelopeViolation: fuserRuns === 1 ? "no top-level JSON object" : undefined,
				};
			},
		});
		const result = await runGraph({
			allowUnguardedWrites: true,
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: workflow.phases,
		});

		expect(result.status).toBe("accepted");
		expect(result.phases[0]?.attempts).toBe(2);
		expect(fuserRuns).toBe(2);
		// The panel was not what got rejected: two seats, polled once.
		expect(owners.filter(owner => owner === "reviewer")).toHaveLength(1);
		expect(owners.filter(owner => owner === "security-reviewer")).toHaveLength(1);
	});

	it("marks a failed seat instead of dropping its slot", async () => {
		await makeDirs();
		let fuserOpinions: Array<{ owner: string; failed: boolean }> = [];
		const workflow = loadWorkflowConfig(FUSION_YAML, {
			runAgent: async context => {
				if (context.owner === "security-reviewer") {
					return { changedFiles: [], label: "seat", exitCode: 1, output: "seat crashed" };
				}
				if (context.owner === "task") {
					fuserOpinions = (context.opinions ?? []).map(opinion => ({
						owner: opinion.owner,
						failed: opinion.failed,
					}));
				}
				return { changedFiles: [], label: context.owner, exitCode: 0, output: "opinion" };
			},
		});
		await runGraph({
			allowUnguardedWrites: true,
			workflowId: "wf",
			runDir,
			workspace: root,
			phases: workflow.phases,
		});

		expect(fuserOpinions).toEqual([
			{ owner: "reviewer", failed: false },
			{ owner: "security-reviewer", failed: true },
		]);
	});
});
