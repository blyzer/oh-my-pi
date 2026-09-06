import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, describe, expect, it } from "bun:test";
import {
	type AdwHost,
	AdwRunError,
	buildSeatSpawnOptions,
	runAdw,
	type SeatOutcome,
	type SeatRequest,
} from "@oh-my-pi/pi-coding-agent/adw/runner";
import { ENVELOPE_CONTRACT } from "@oh-my-pi/pi-coding-agent/adw/prompt";
import type { AdwWorkflowConfig } from "@oh-my-pi/pi-coding-agent/adw/types";
import type { AgentDefinition } from "@oh-my-pi/pi-coding-agent/task/types";

const roots: string[] = [];

function tempDir(label: string): string {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), `adw-${label}-`));
	roots.push(dir);
	return dir;
}

afterEach(() => {
	for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

const AGENT: AgentDefinition = { name: "sonic", description: "d", systemPrompt: "s", source: "bundled" };

function envelope(fields: Record<string, unknown> = {}): string {
	return `thinking out loud\n\n${JSON.stringify({ status: "success", summary: "done", ...fields })}`;
}

function outcome(output: string, extra: Partial<SeatOutcome> = {}): SeatOutcome {
	return { output, stderr: "", exitCode: 0, tokens: 100, model: "test/model", ...extra };
}

/** Records every seat request and replies with a scripted outcome per call. */
function scripted(replies: ((request: SeatRequest, call: number) => SeatOutcome)[]) {
	const seen: SeatRequest[] = [];
	let call = 0;
	return {
		seen,
		runner: async (request: SeatRequest): Promise<SeatOutcome> => {
			seen.push(request);
			const reply = replies[Math.min(call, replies.length - 1)];
			call++;
			if (!reply) throw new Error("no scripted reply");
			return reply(request, call - 1);
		},
	};
}

const AGENT_PHASE: AdwWorkflowConfig = {
	name: "solo",
	maxAttempts: 3,
	phases: [{ name: "write", kind: "agent", owner: "sonic", gates: ["artifacts_exist"] }],
};

const FUSION_PHASE: AdwWorkflowConfig = {
	name: "fused",
	maxAttempts: 3,
	phases: [
		{
			name: "decide",
			kind: "fusion",
			gates: ["artifacts_exist"],
			panel: [{ owner: "sonic" }, { owner: "sonic", model: "other/model" }],
			fuser: { owner: "sonic" },
		},
	],
};

function host(cwd: string): AdwHost {
	return { cwd };
}

describe("retry decisions", () => {
	it("continues the same seat on a retry instead of starting a new one", async () => {
		const cwd = tempDir("retry");
		// Attempt 1 claims a file it never wrote; attempt 2 writes it first.
		const script = scripted([
			() => outcome(envelope({ artifacts: ["out.md"] })),
			() => {
				fs.writeFileSync(path.join(cwd, "out.md"), "content");
				return outcome(envelope({ artifacts: ["out.md"] }));
			},
		]);

		const result = await runAdw({
			host: host(cwd),
			workflow: AGENT_PHASE,
			request: "make it",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		expect(script.seen).toHaveLength(2);
		// Same seat id across attempts — that is what lets the executor continue
		// the session rather than cold-start one that never saw its own answer.
		expect(script.seen[1]?.id).toBe(script.seen[0]?.id as string);
		expect(script.seen[0]?.followUpMessage).toBeUndefined();
		// The retry carries the violation, and names the artifact that was missing.
		expect(script.seen[1]?.followUpMessage).toContain("out.md");
	});

	it("stops asking once the attempt budget is spent", async () => {
		const cwd = tempDir("exhaust");
		const script = scripted([() => outcome(envelope({ artifacts: ["never.md"] }))]);

		const result = await runAdw({
			host: host(cwd),
			workflow: { ...AGENT_PHASE, maxAttempts: 2 },
			request: "make it",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(false);
		expect(script.seen).toHaveLength(2);
		expect(result.summary.phases[0]?.violations.join(" ")).toContain("never.md");
	});

	it("treats a crashed seat as a failed phase rather than parsing its output", async () => {
		const cwd = tempDir("crash");
		const script = scripted([() => outcome("", { exitCode: 1, stderr: "provider 429" })]);

		const result = await runAdw({
			host: host(cwd),
			workflow: { ...AGENT_PHASE, maxAttempts: 1 },
			request: "make it",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(false);
		expect(result.summary.phases[0]?.violations.join(" ")).toContain("provider 429");
	});
});

describe("fusion decisions", () => {
	it("polls every panel seat once, then the fuser", async () => {
		const cwd = tempDir("fusion");
		const script = scripted([
			() => outcome("opinion A"),
			() => outcome("opinion B"),
			() => {
				fs.writeFileSync(path.join(cwd, "verdict.md"), "fused");
				return outcome(envelope({ artifacts: ["verdict.md"] }));
			},
		]);

		const result = await runAdw({
			host: host(cwd),
			workflow: FUSION_PHASE,
			request: "decide",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		expect(script.seen).toHaveLength(3);
		expect(script.seen.filter(request => request.readOnly)).toHaveLength(2);
		// The writer is last and is the only seat allowed to write.
		expect(script.seen[2]?.readOnly).toBe(false);
		expect(script.seen[2]?.id).toContain("fuser");
	});

	it("re-runs only the fuser on a retry — the panel's opinions were not rejected", async () => {
		const cwd = tempDir("fusion-retry");
		let fuserCalls = 0;
		const script = scripted([
			() => outcome("opinion A"),
			() => outcome("opinion B"),
			// Fuser: claims an artifact, then writes it on the retry.
			request => {
				if (!request.readOnly) fuserCalls++;
				if (fuserCalls === 2) fs.writeFileSync(path.join(cwd, "verdict.md"), "fused");
				return outcome(envelope({ artifacts: ["verdict.md"] }));
			},
		]);

		const result = await runAdw({
			host: host(cwd),
			workflow: FUSION_PHASE,
			request: "decide",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		// Two panel polls total, not four: re-polling N models to receive the
		// same answers is the most expensive way to change nothing.
		expect(script.seen.filter(request => request.readOnly)).toHaveLength(2);
		expect(fuserCalls).toBe(2);
		expect(script.seen.at(-1)?.followUpMessage).toContain("verdict.md");
	});

	it("labels each opinion with the model that answered, and survives a dead seat", async () => {
		const cwd = tempDir("fusion-dead");
		const fuserPrompts: string[] = [];
		const script = scripted([
			() => outcome("opinion A", { model: "resolved/one" }),
			() => {
				throw new Error("seat exploded");
			},
			request => {
				fuserPrompts.push(request.task);
				fs.writeFileSync(path.join(cwd, "verdict.md"), "fused");
				return outcome(envelope({ artifacts: ["verdict.md"] }));
			},
		]);

		const result = await runAdw({
			host: host(cwd),
			workflow: FUSION_PHASE,
			request: "decide",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		// The surviving opinion is labelled with the model that actually answered.
		expect(fuserPrompts[0]).toContain("resolved/one");
		// The dead one is named as failed rather than quietly omitted.
		expect(fuserPrompts[0]).toContain("FAILED");
		expect(fuserPrompts[0]).toContain("seat exploded");
	});

	it("fails the phase when every panel seat dies", async () => {
		const cwd = tempDir("fusion-allDead");
		const script = scripted([
			() => {
				throw new Error("boom");
			},
		]);

		const result = await runAdw({
			host: host(cwd),
			workflow: { ...FUSION_PHASE, maxAttempts: 1 },
			request: "decide",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(false);
		expect(result.summary.phases[0]?.violations.join(" ")).toContain("every panel seat failed");
		// The fuser is never asked to merge nothing.
		expect(script.seen.every(request => request.readOnly)).toBe(true);
	});
});

describe("seat agent derivation", () => {
	/** The agents that actually reach the executor for a fusion phase. */
	async function seatAgents(cwd: string) {
		const script = scripted([
			() => outcome("opinion A"),
			() => outcome("opinion B"),
			() => {
				fs.writeFileSync(path.join(cwd, "verdict.md"), "fused");
				return outcome(envelope({ artifacts: ["verdict.md"] }));
			},
		]);
		await runAdw({ host: host(cwd), workflow: FUSION_PHASE, request: "decide", seatRunner: script.runner });
		return { panel: script.seen.filter(r => r.readOnly), fuser: script.seen.find(r => !r.readOnly) };
	}

	it("gives panel seats a read-only toolset and no envelope contract", async () => {
		const { panel } = await seatAgents(tempDir("derive-panel"));
		expect(panel).toHaveLength(2);
		for (const seat of panel) {
			// Single-writer, enforced in the toolset rather than asked for politely.
			expect(seat.agent.tools).toBeDefined();
			for (const writeTool of ["write", "edit", "bash", "task"]) {
				expect(seat.agent.tools).not.toContain(writeTool);
			}
			expect(seat.agent.tools).toContain("read");
			// `yield` is required of any subagent with an explicit tool list, and
			// hand-built definitions do not get it appended for them.
			expect(seat.agent.tools).toContain("yield");
			// An opinion is prose: asking for JSON too invites a claim of work done.
			expect(seat.agent.systemPrompt).not.toContain(ENVELOPE_CONTRACT);
			// No fanning out further from inside a panel seat.
			expect(seat.agent.spawns).toBeUndefined();
		}
	});

	it("gives the fuser the envelope contract and leaves its tools alone", async () => {
		const { fuser } = await seatAgents(tempDir("derive-fuser"));
		expect(fuser?.agent.systemPrompt).toContain(ENVELOPE_CONTRACT);
		// Untouched: the roster decides what the writer may do.
		expect(fuser?.agent.tools).toBe(AGENT.tools);
	});
});

describe("buildSeatSpawnOptions", () => {
	function request(readOnly: boolean): SeatRequest {
		return {
			seat: "sonic",
			agent: AGENT,
			task: "t",
			id: "seat-1",
			index: 0,
			description: "d",
			assignment: "a",
			readOnly,
		};
	}
	const base = {
		host: host("/repo"),
		workRoot: "/sandbox",
		artifactsDir: "/art",
		modelPatterns: ["m"],
		modelRole: undefined,
	};

	it("denies a panel seat the peer roster — the independence invariant", () => {
		// `restrictToolNames` removes the `hub` tool but NOT the IRC prompt, which
		// names sibling seats and what each is currently investigating.
		const options = buildSeatSpawnOptions({ ...base, seat: request(true) });
		expect(options.enableIrc).toBe(false);
		expect(options.restrictToolNames).toBe(true);
	});

	it("leaves a writer seat addressable", () => {
		expect(buildSeatSpawnOptions({ ...base, seat: request(false) }).enableIrc).toBe(true);
	});

	it("never enables LSP for a read-only seat", () => {
		expect(buildSeatSpawnOptions({ ...base, seat: request(true) }).enableLsp).toBe(false);
	});

	it("runs the seat in the sandbox, not the real checkout", () => {
		expect(buildSeatSpawnOptions({ ...base, seat: request(false) }).cwd).toBe("/sandbox");
	});

	it("shares the parent artifact manager so concurrent seats cannot collide", () => {
		const manager = { marker: true } as never;
		const options = buildSeatSpawnOptions({
			...base,
			host: { cwd: "/repo", artifactManager: manager },
			seat: request(true),
		});
		expect(options.parentArtifactManager).toBe(manager);
	});
});

describe("resume decisions", () => {
	const TWO_PHASE: AdwWorkflowConfig = {
		name: "twostep",
		maxAttempts: 2,
		phases: [
			{ name: "plan", kind: "agent", owner: "sonic", gates: ["artifacts_exist"] },
			{ name: "build", kind: "agent", owner: "sonic" },
		],
	};

	it("continues at the phase that was in flight and keeps the handoff", async () => {
		const cwd = tempDir("resume");
		// A session file makes the run dir deterministic, so the second call can
		// find the first run's trace.
		const sessionFile = path.join(cwd, "session.jsonl");
		const runHost: AdwHost = { cwd, sessionFile };

		fs.writeFileSync(path.join(cwd, "plan.md"), "plan");
		const first = scripted([
			() => outcome(envelope({ artifacts: ["plan.md"], notes_for_next_agent: "read plan.md first" })),
			() => {
				throw new Error("process died mid-build");
			},
		]);
		const failed = await runAdw({
			host: runHost,
			workflow: TWO_PHASE,
			request: "ship it",
			seatRunner: first.runner,
		}).catch((err: unknown) => err);
		expect(failed).toBeInstanceOf(Error);

		const second = scripted([() => outcome(envelope())]);
		const resumed = await runAdw({
			host: { cwd, sessionFile: path.join(cwd, "another-session.jsonl") },
			workflow: TWO_PHASE,
			request: "",
			// A DIFFERENT session file: a crashed run is resumed by a new process,
			// so run state must not be keyed by the session that died.
			resumeAdwId: (failed as AdwRunError).adwId,
			seatRunner: second.runner,
		});

		expect(resumed.summary.accepted).toBe(true);
		// `plan` passed before the crash; only `build` runs on the resume.
		expect(second.seen).toHaveLength(1);
		expect(second.seen[0]?.id).toContain("build");
		// The handoff came back from disk — the events alone cannot carry it.
		expect(second.seen[0]?.task).toContain("read plan.md first");
		// And the request was read back rather than re-supplied.
		expect(second.seen[0]?.task).toContain("ship it");
		expect(resumed.summary.phases.map(phase => phase.name)).toEqual(["plan", "build"]);
	});

	it("refuses to resume an isolated run whose sandbox is gone", async () => {
		const cwd = tempDir("resume-iso");
		const error = await runAdw({
			host: { cwd, sessionFile: path.join(cwd, "s.jsonl") },
			workflow: { ...TWO_PHASE, isolation: true },
			request: "",
			resumeAdwId: "adw-does-not-matter",
			seatRunner: scripted([() => outcome(envelope())]).runner,
		}).catch((err: unknown) => err);
		expect((error as Error).message).toContain("cannot be resumed");
	});
});
