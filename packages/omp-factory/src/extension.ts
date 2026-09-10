/**
 * omp-factory probe: minimal external Factory controller.
 *
 * An external extension launches one managed builder through the public
 * `runSubprocess` policy path (via the injected `pi.pi` namespace) and hands
 * the candidate to the workflow driver, which owns acceptance — no core
 * change, no vendored runtime.
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type { AgentDefinition, AgentSource, ExtensionAPI, ExtensionCommandContext } from "@oh-my-pi/pi-coding-agent";
import { captureBaselineState, captureTouchedSince } from "./capture";
import { runWorkflow } from "./workflow";

const BUILDER_SYSTEM_PROMPT = [
	"You are a factory builder.",
	"Follow the assignment exactly.",
	"Reply concisely with what you did.",
].join("\n");

export function buildProbeAssignment(args: string): string {
	const trimmed = args.trim();
	return trimmed.length > 0 ? trimmed : "Reply with exactly: FACTORY-PROBE-OK";
}

function builderAgent(): AgentDefinition {
	return {
		name: "factory-builder",
		description: "Factory probe builder",
		systemPrompt: BUILDER_SYSTEM_PROMPT,
		source: "project" as AgentSource,
	};
}

export default function factoryExtension(pi: ExtensionAPI): void {
	pi.registerCommand("factory", {
		description: "Run a minimal Factory single-builder probe",
		handler: async (args: string, ctx: ExtensionCommandContext) => {
			const assignment = buildProbeAssignment(args);
			const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-probe-"));
			const workflowId = `factory-probe-${Date.now()}`;
			const result = await runWorkflow({
				workflowId,
				phase: "probe",
				runDir,
				workspace: ctx.cwd,
				scope: ["**"],
				assertions: [],
				maxAttempts: 1,
				produce: async () => {
					const state = await captureBaselineState(ctx.cwd);
					const spawned = await pi.pi.runSubprocess({
						cwd: ctx.cwd,
						agent: builderAgent(),
						task: assignment,
						index: 0,
						id: workflowId,
						modelRegistry: ctx.modelRegistry,
					});
					return {
						changedFiles: await captureTouchedSince(state),
						label: workflowId,
						exitCode: spawned.exitCode,
						output: spawned.output,
					};
				},
			});
			const kind = result.status === "accepted" ? "info" : "error";
			ctx.ui.notify(
				`factory ${result.status} after ${result.attempts} attempt(s) ledger=${runDir} ${result.evidence.join(" | ").slice(0, 300)}`,
				kind,
			);
		},
	});
}
