/**
 * omp-factory probe: minimal external Factory controller.
 *
 * An external extension launches one managed builder through the public
 * `runSubprocess` policy path and hands the candidate to the workflow driver,
 * which owns acceptance — no core change, no vendored runtime.
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { captureBaselineState, captureTouchedSince } from "./capture";
import { runWorkflow } from "./workflow";

// Minimal structural types (scratch scaffold; repo conventions apply on merge).
interface FactoryPi {
	registerCommand(
		name: string,
		def: {
			description: string;
			handler: (args: string, ctx: FactoryCtx) => Promise<void>;
		},
	): void;
	runSubprocess(options: Record<string, unknown>): Promise<{
		exitCode: number;
		output: string;
		error?: string;
	}>;
}

interface FactoryCtx {
	cwd: string;
	modelRegistry: unknown;
	ui: {
		notify(message: string, kind: "info" | "error"): void;
	};
}

const BUILDER_SYSTEM_PROMPT = [
	"You are a factory builder.",
	"Follow the assignment exactly.",
	"Reply concisely with what you did.",
].join("\n");

export function buildProbeAssignment(args: string): string {
	const trimmed = args.trim();
	return trimmed.length > 0 ? trimmed : "Reply with exactly: FACTORY-PROBE-OK";
}

export default function factoryExtension(pi: FactoryPi): void {
	pi.registerCommand("factory", {
		description: "Run a minimal Factory single-builder probe",
		handler: async (args: string, ctx: FactoryCtx) => {
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
					const spawned = await pi.runSubprocess({
						cwd: ctx.cwd,
						agent: {
							name: "factory-builder",
							description: "Factory probe builder",
							systemPrompt: BUILDER_SYSTEM_PROMPT,
							source: "project",
						},
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
