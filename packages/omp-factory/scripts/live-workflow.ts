/**
 * Run a frozen-format ADW workflow file end to end against live models.
 *
 * Usage: bun scripts/live-workflow.ts <workflow.yml> <git-workspace> <request>
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { discoverAuthStorage } from "@oh-my-pi/pi-coding-agent";
import { ModelRegistry } from "@oh-my-pi/pi-coding-agent/config/model-registry";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { type ExecutorOptions, runSubprocess } from "@oh-my-pi/pi-coding-agent/task/executor";
import { captureBaselineState, captureTouchedSince } from "../src/capture";
import { parseEnvelope } from "../src/envelope";
import { runGraph } from "../src/graph";
import { copyIsolation } from "../src/isolation";
import { replay } from "../src/ledger";
import { loadWorkflowConfig } from "../src/workflow-config";

const [configPath, workspace, ...requestParts] = Bun.argv.slice(2);
const request = requestParts.join(" ");
if (!configPath || !workspace || request.length === 0) {
	console.error("usage: live-workflow.ts <workflow.yml> <git-workspace> <request>");
	process.exit(2);
}

const ENVELOPE_RULE = [
	"End your final message with one JSON object and nothing after it:",
	'{"status":"success"|"fail","summary":"one line","artifacts":["repo-relative/path/you/wrote"]}',
	"Declare every file you changed. Undeclared writes are rejected.",
].join("\n");

const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-live-run-"));
const journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-live-journal-"));
const workflowId = `live-${Date.now()}`;
const authStorage = await discoverAuthStorage();
const modelRegistry = new ModelRegistry(authStorage);
const settings = Settings.isolated();
let spawnCount = 0;

try {
	const workflow = loadWorkflowConfig(await Bun.file(configPath).text(), {
		request,
		runAgent: async ({
			phase,
			owner,
			model,
			thinking,
			prompt,
			correction,
			inputs,
			workspace: dir,
			readOnly,
			opinions,
		}) => {
			const state = await captureBaselineState(dir);
			const task = [
				prompt,
				inputs.length > 0
					? `Accepted inputs: ${inputs.map(input => `${input.phase} v${input.version} (${input.artifacts.join(", ")})`).join("; ")}`
					: "",
				opinions && opinions.length > 0
					? `Panel opinions, in declared order:\n${opinions
							.map(op => `--- ${op.owner}${op.failed ? " (seat failed)" : ""} ---\n${op.text}`)
							.join("\n\n")}`
					: "",
				correction ? `Previous attempt was rejected: ${correction}` : "",
			]
				.filter(part => part.trim().length > 0)
				.join("\n\n");
			const spawned = await runSubprocess({
				cwd: dir,
				agent: {
					name: owner,
					description: `Factory ${phase} phase`,
					// A panel seat owes an opinion, not a result: no write, no
					// edit, no bash. That is what makes running them at once safe.
					tools: readOnly ? ["read", "grep", "glob", "yield"] : undefined,
					systemPrompt: readOnly
						? `You are a read-only panel seat in the ${phase} phase. Answer the question; change nothing on disk.`
						: `You are the ${phase} phase of a software factory.\n\n${ENVELOPE_RULE}`,
					source: "project",
				},
				task,
				index: 0,
				// Every spawn needs its own registry id: a revision re-runs the
				// same phase and owner, and a duplicate id fails the spawn.
				id: `${workflowId}-${phase}-${owner}-${(spawnCount += 1)}`,
				modelOverride: model,
				thinkingLevel: thinking as ExecutorOptions["thinkingLevel"],
				modelRegistry,
				settings,
			});
			if (readOnly) {
				// A seat's answer is evidence for the fuser, not a candidate.
				return {
					changedFiles: [],
					label: `${phase}-seat-${owner}`,
					exitCode: spawned.exitCode,
					output: spawned.output,
				};
			}
			const envelope = parseEnvelope(spawned.output ?? "");
			return {
				changedFiles: await captureTouchedSince(state),
				label: `${phase}-${spawned.exitCode}`,
				exitCode: spawned.exitCode,
				output: spawned.output,
				envelopeViolation: envelope.ok ? undefined : envelope.violation,
				selfReportedStatus: envelope.ok ? envelope.envelope.status : undefined,
				summary: envelope.ok ? envelope.envelope.summary : undefined,
				declaredArtifacts: envelope.ok ? envelope.envelope.artifacts : undefined,
			};
		},
	});

	console.log(`workflow=${workflow.name} isolation=${workflow.isolation} phases=${workflow.phases.length}`);
	const result = await runGraph({
		workflowId,
		runDir,
		workspace,
		phases: workflow.phases,
		protectedGlobs: workflow.protectedGlobs,
		isolation: workflow.isolation ? copyIsolation() : undefined,
		integrate: workflow.isolation ? { journalDir, base: workflowId } : undefined,
	});

	for (const outcome of result.phases) {
		console.log(
			`phase=${outcome.phase} status=${outcome.status} attempts=${outcome.attempts} inputs=${outcome.inputs
				.map(input => `${input.phase}v${input.version}`)
				.join(",")} ${outcome.evidence.join(" | ").slice(0, 240)}`,
		);
	}
	const { projection } = await replay(runDir);
	console.log(`graph=${result.status} ledger=${projection.status} order=${result.order.join(" -> ")}`);
	console.log(`runDir=${runDir}`);
	process.exit(result.status === "accepted" ? 0 : 1);
} finally {
	await fs.rm(journalDir, { recursive: true, force: true });
}
