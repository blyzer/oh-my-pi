/**
 * ADW spawns many short-lived sessions per run: every panel seat, fuser, writer
 * and the router. Each must rebind the parent session's already-imported
 * extension factories rather than rediscover and re-import every extension
 * module graph — the same contract upstream's task spawner follows.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, describe, expect, it, vi } from "bun:test";
import { classifyWorkflow } from "@oh-my-pi/pi-coding-agent/adw/classify";
import { type AdwHost, createExecutorSeatRunner, runAdw } from "@oh-my-pi/pi-coding-agent/adw/runner";
import type { AdwWorkflowConfig, DiscoveredWorkflow } from "@oh-my-pi/pi-coding-agent/adw/types";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import type { PreparedExtension } from "@oh-my-pi/pi-coding-agent/extensibility/extensions/types";
import type { AgentSession } from "@oh-my-pi/pi-coding-agent/session/agent-session";
import type { SessionManager } from "@oh-my-pi/pi-coding-agent/session/session-manager";
import { BUILTIN_ADW_SLASH_COMMANDS } from "@oh-my-pi/pi-coding-agent/slash-commands/builtin-adw";
import type { SlashCommandRuntime } from "@oh-my-pi/pi-coding-agent/slash-commands/types";
import * as executorModule from "@oh-my-pi/pi-coding-agent/task/executor";
import type { SingleResult } from "@oh-my-pi/pi-tui/tools/task";

const roots: string[] = [];

function tempDir(label: string): string {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), `adw-${label}-`));
	roots.push(dir);
	return dir;
}

afterEach(() => {
	vi.restoreAllMocks();
	for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

function prepared(): PreparedExtension[] {
	return [
		{
			path: "/parent/.omp/extensions/hook.ts",
			resolvedPath: "/parent/.omp/extensions/hook.ts",
			factory: () => {},
			error: null,
		},
	];
}

function result(id: string, output: string): SingleResult {
	return {
		index: 0,
		id,
		agent: "sonic",
		agentSource: "bundled",
		task: "t",
		exitCode: 0,
		output,
		stderr: "",
		truncated: false,
		durationMs: 1,
		tokens: 0,
		requests: 1,
	};
}

const ENVELOPE = `done\n\n${JSON.stringify({ status: "success", summary: "done" })}`;

/** One writer phase and one fusion phase: writer, two read-only panel seats, fuser. */
const WORKFLOW: AdwWorkflowConfig = {
	name: "mixed",
	maxAttempts: 1,
	phases: [
		{ name: "build", kind: "agent", owner: "sonic" },
		{ name: "decide", kind: "fusion", panel: [{ owner: "sonic" }, { owner: "sonic" }], fuser: { owner: "sonic" } },
	],
};

describe("ADW seats reuse the parent's prepared extensions", () => {
	it("hands the same prepared set to every seat of a run ", async () => {
		const cwd = tempDir("prepared");
		const preparedExtensions = prepared();
		const spawn = vi
			.spyOn(executorModule, "runSubprocess")
			.mockImplementation(async options => result(options.id, ENVELOPE));

		await runAdw({ host: { cwd, preparedExtensions }, workflow: WORKFLOW, request: "do it" });

		// Writer, both panel seats and the fuser: every session the run created.
		expect(spawn.mock.calls.length).toBeGreaterThanOrEqual(4);
		const readOnly = spawn.mock.calls.filter(([options]) => options.restrictToolNames === true);
		expect(readOnly.length).toBe(2);
		for (const [options] of spawn.mock.calls) {
			// Identity, not equality: the parent imported these once; seats only rebind.
			expect(options.preloadedPreparedExtensions).toBe(preparedExtensions);
		}
	});

	it("lets a sandboxed run re-discover inside its sandbox, as an isolated task does", async () => {
		const preparedExtensions = prepared();
		const spawn = vi
			.spyOn(executorModule, "runSubprocess")
			.mockImplementation(async options => result(options.id, ENVELOPE));
		const runSeat = createExecutorSeatRunner({
			host: { cwd: "/repo", preparedExtensions },
			workRoot: "/sandbox",
			artifactsDir: tempDir("sandbox-art"),
		});

		await runSeat({
			seat: "sonic",
			agent: { name: "sonic", description: "d", systemPrompt: "s", source: "bundled" },
			task: "t",
			id: "seat-1",
			index: 0,
			description: "d",
			assignment: "a",
			readOnly: false,
		});

		expect(spawn.mock.calls[0]?.[0].cwd).toBe("/sandbox");
		expect(spawn.mock.calls[0]?.[0].preloadedPreparedExtensions).toBeUndefined();
	});
});

function workflow(name: string): DiscoveredWorkflow {
	return {
		workflow: { name, phases: [{ name: "p", kind: "agent", owner: "task" }] },
		path: `/tmp/${name}.yml`,
		level: "project",
	} as DiscoveredWorkflow;
}

describe("the ADW router reuses the parent's prepared extensions", () => {
	it("forwards the host's prepared set to its restricted session", async () => {
		const preparedExtensions = prepared();
		const spawn = vi
			.spyOn(executorModule, "runSubprocess")
			.mockImplementation(async options => result(options.id, JSON.stringify({ workflow: "fix", reason: "r" })));
		const host: AdwHost = { cwd: tempDir("router"), preparedExtensions };

		const choice = await classifyWorkflow("repair it", [workflow("fix"), workflow("ship")], { host });

		expect(choice.workflow?.workflow.name).toBe("fix");
		expect(spawn).toHaveBeenCalledTimes(1);
		expect(spawn.mock.calls[0]?.[0].restrictToolNames).toBe(true);
		expect(spawn.mock.calls[0]?.[0].preloadedPreparedExtensions).toBe(preparedExtensions);
	});

	it("takes the prepared set from the session that ran /adw", async () => {
		const cwd = tempDir("slash");
		const adwDir = path.join(cwd, ".omp", "adw");
		fs.mkdirSync(adwDir, { recursive: true });
		for (const name of ["fix", "ship"]) {
			fs.writeFileSync(
				path.join(adwDir, `${name}.yml`),
				`name: ${name}\nphases:\n  - name: p\n    kind: agent\n    owner: task\n`,
			);
		}
		const preparedExtensions = prepared();
		const spawn = vi
			.spyOn(executorModule, "runSubprocess")
			.mockImplementation(async options => result(options.id, JSON.stringify({ workflow: null, reason: "none" })));
		const session = {
			modelRegistry: { authStorage: {} },
			model: undefined,
			getAgentId: () => undefined,
			preparedExtensions,
		} as unknown as AgentSession;
		const sessionManager = {
			getSessionFile: () => undefined,
			getArtifactManager: () => undefined,
			getAdditionalDirectories: () => [],
		} as unknown as SessionManager;
		const output: string[] = [];
		const command = BUILTIN_ADW_SLASH_COMMANDS.find(spec => spec.name === "adw");

		await command?.handle?.({ name: "adw", args: "tidy the release notes", text: "/adw tidy the release notes" }, {
			cwd,
			settings: Settings.isolated(),
			session,
			sessionManager,
			output: (text: string) => {
				output.push(text);
			},
		} as unknown as SlashCommandRuntime);

		expect(output.join("\n")).toContain("No workflow fits");
		expect(spawn).toHaveBeenCalledTimes(1);
		expect(spawn.mock.calls[0]?.[0].preloadedPreparedExtensions).toBe(preparedExtensions);
	});
});
