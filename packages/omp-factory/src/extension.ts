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
import { parseEnvelope } from "./envelope";
import { runGraph } from "./graph";
import { nativeWriteGuard } from "./write-guard";

const BUILDER_SYSTEM_PROMPT = [
	"You are a factory builder.",
	"Follow the assignment exactly.",
	"End your final message with one JSON object:",
	'{"status":"success"|"fail","summary":"one line","artifacts":["repo-relative/path/you/wrote"]}',
	"Declare every file you changed. Undeclared writes are rejected.",
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
		description: "Run a Factory workflow for the request",
		handler: async (args: string, ctx: ExtensionCommandContext) => {
			const assignment = buildProbeAssignment(args);
			const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-probe-"));
			const workflowId = `factory-probe-${Date.now()}`;
			// One writer phase today. The command and the engine share this one
			// path, so a multi-phase workflow is a longer `phases` list, never a
			// second acceptance implementation.
			//
			// The phase's entry state is taken before anything is dispatched: a
			// baseline captured inside an attempt measures the tree after an
			// earlier attempt's writes.
			const state = await captureBaselineState(ctx.cwd);
			const result = await runGraph({
				// `/factory` writes straight into the operator's checkout, so a
				// rejected attempt must be undone rather than merely refused.
				writeGuard: nativeWriteGuard(),
				workflowId,
				runDir,
				workspace: ctx.cwd,
				phases: [
					{
						name: "build",
						scope: ["**"],
						assertions: [],
						maxAttempts: 1,
						writes: true,
						produce: async () => {
							const spawned = await pi.pi.runSubprocess({
								cwd: ctx.cwd,
								agent: builderAgent(),
								task: assignment,
								index: 0,
								id: workflowId,
								modelRegistry: ctx.modelRegistry,
							});
							const envelope = parseEnvelope(spawned.output ?? "");
							return {
								changedFiles: await captureTouchedSince(state),
								label: workflowId,
								exitCode: spawned.exitCode,
								output: spawned.output,
								envelopeViolation: envelope.ok ? undefined : envelope.violation,
								selfReportedStatus: envelope.ok ? envelope.envelope.status : undefined,
								summary: envelope.ok ? envelope.envelope.summary : undefined,
								declaredArtifacts: envelope.ok ? envelope.envelope.artifacts : undefined,
							};
						},
					},
				],
			});
			const build = result.phases.find(outcome => outcome.phase === "build");
			const kind = result.status === "accepted" ? "info" : "error";
			ctx.ui.notify(
				`factory ${result.status} after ${build?.attempts ?? 0} attempt(s) ledger=${runDir} ${(build?.evidence ?? []).join(" | ").slice(0, 300)}`,
				kind,
			);
		},
	});
}
