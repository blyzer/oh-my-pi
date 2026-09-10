/**
 * WI-0024 against a live model: a verification config demanding both
 * `file_contains(marker)` and `file_not_contains(marker)` is impossible to
 * satisfy. A real builder runs, may well succeed at its task, and the run
 * still must reject and land nothing.
 *
 * Usage: bun scripts/live-wi0024.ts <git-workspace>
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { discoverAuthStorage } from "@oh-my-pi/pi-coding-agent";
import { ModelRegistry } from "@oh-my-pi/pi-coding-agent/config/model-registry";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { runSubprocess } from "@oh-my-pi/pi-coding-agent/task/executor";
import { captureBaselineState, captureTouchedSince } from "../src/capture";
import { parseEnvelope } from "../src/envelope";
import { runGraph } from "../src/graph";
import { copyIsolation } from "../src/isolation";
import { replay } from "../src/ledger";

const workspace = Bun.argv[2];
if (!workspace) {
	console.error("usage: live-wi0024.ts <git-workspace>");
	process.exit(2);
}

const MARKER = "WI-0024-MARKER";
const runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-wi0024-"));
const journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-wi0024-journal-"));
const workflowId = `wi0024-${Date.now()}`;
const authStorage = await discoverAuthStorage();
const modelRegistry = new ModelRegistry(authStorage);
const settings = Settings.isolated();

try {
	const result = await runGraph({
		workflowId,
		runDir,
		workspace,
		// Without a sandbox the builder writes straight into the shared tree,
		// so a rejected attempt still leaves its files behind. Isolation is
		// what makes "nothing landed" true of the builder, not just of the
		// integration journal.
		isolation: copyIsolation(),
		integrate: { journalDir, base: "wi0024-base" },
		phases: [
			{
				name: "build",
				scope: ["**"],
				writes: true,
				maxAttempts: 1,
				// The contradiction. No tree can satisfy both.
				assertions: [
					{ type: "file_contains", file: "marker.txt", marker: MARKER },
					{ type: "file_not_contains", file: "marker.txt", marker: MARKER },
				],
				produce: async ({ workspace: dir }) => {
					const state = await captureBaselineState(dir);
					const spawned = await runSubprocess({
						cwd: dir,
						agent: {
							name: "factory-builder",
							description: "WI-0024 live builder",
							systemPrompt: [
								"You are a factory builder.",
								"End your final message with one JSON object:",
								'{"status":"success"|"fail","summary":"one line","artifacts":["path"]}',
							].join("\n"),
							source: "project",
						},
						task: `Write the file marker.txt containing exactly ${MARKER}. Then declare it.`,
						index: 0,
						id: workflowId,
						modelRegistry,
						settings,
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

	const { projection } = await replay(runDir);
	const landed = await Bun.file(path.join(workspace, "marker.txt"))
		.text()
		.then(() => true)
		.catch(() => false);
	const journal = await Bun.file(path.join(journalDir, "integration.json"))
		.json()
		.catch(() => null);

	console.log(`graph=${result.status}`);
	console.log(`phase=${result.phases[0]?.status} evidence=${result.phases[0]?.evidence.join(" | ")}`);
	console.log(`ledger=${projection.status} acceptedVersion=${projection.phases.build?.acceptedVersion}`);
	console.log(`landedInRoot=${landed} journal=${journal === null ? "none" : "present"}`);

	const ok =
		result.status === "failed" &&
		projection.status === "failed" &&
		projection.phases.build?.acceptedVersion === null &&
		!landed &&
		journal === null;
	console.log(ok ? "WI-0024-LIVE-OK (impossible verification never accepted)" : "WI-0024-LIVE-FAILED");
	process.exit(ok ? 0 : 1);
} finally {
	await fs.rm(runDir, { recursive: true, force: true });
	await fs.rm(journalDir, { recursive: true, force: true });
}
