import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { $ } from "bun";
import { afterEach, describe, expect, it } from "bun:test";
import {
	type AdwHost,
	AdwRunError,
	buildSeatSpawnOptions,
	runAdw,
	type SeatOutcome,
	type SeatRequest,
	settleIsolation,
} from "@oh-my-pi/pi-coding-agent/adw/runner";
import { ENVELOPE_CONTRACT_BUNDLED } from "@oh-my-pi/pi-coding-agent/adw/prompt";
import type { AdwWorkflowConfig } from "@oh-my-pi/pi-coding-agent/adw/types";
import { REVIEW_JSON_SCHEMA, VERDICT_GATE } from "@oh-my-pi/pi-coding-agent/adw/schema";
import { TaskTraceReader, taskTraceLayout } from "@oh-my-pi/pi-natives";
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

const REVIEW_PHASE: AdwWorkflowConfig = {
	name: "review",
	maxAttempts: 3,
	acceptance: "review",
	phases: [{ name: "review", kind: "agent", owner: "sonic", schema: REVIEW_JSON_SCHEMA, gates: [VERDICT_GATE] }],
};

const APPROVED_REVIEW = {
	approved: true,
	blocking: [],
	findings: [{ requirement: "migration preserves records", met: true, evidence: "migration check passed" }],
};
const REJECTED_REVIEW = {
	approved: false,
	blocking: ["migration loses records"],
	findings: [{ requirement: "migration preserves records", met: false, evidence: "migration check lost row 7" }],
};

function terminalEvent(traceDir: string) {
	const reader = new TaskTraceReader(traceDir);
	const layout = taskTraceLayout();
	const records = reader.readRaw(0);
	const strings = reader.strings();
	for (let offset = records.length - layout.recordLen; offset >= 0; offset -= layout.recordLen) {
		if (layout.kindNames[records[offset + layout.offKind] as number] !== "run_finished") continue;
		const detail = records.readUInt32LE(offset + layout.offDetail);
		return {
			accepted: ((records[offset + layout.offFlags] as number) & layout.flagOk) !== 0,
			reason: detail === 0 ? "" : strings[detail - 1],
		};
	}
	throw new Error("run has no terminal trace event");
}

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

	it("corrects a schema-invalid fuser without validating or repeating panel opinions", async () => {
		const script = scripted([
			() => outcome("Panel A recommends preserving all records."),
			() => outcome("Panel B independently agrees."),
			() => outcome(envelope({ decision: 1 })),
			() => outcome(envelope({ decision: "retain" })),
		]);
		const result = await runAdw({
			host: host(tempDir("fusion-schema")),
			workflow: {
				...FUSION_PHASE,
				phases: FUSION_PHASE.phases.map(phase => ({
					...phase,
					schema: { type: "object", required: ["decision"], properties: { decision: { type: "string" } } },
					gates: [],
				})),
			},
			request: "review migration",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		expect(result.summary.phases[0]?.attempts).toBe(2);
		expect(script.seen.filter(request => request.readOnly)).toHaveLength(2);
		expect(script.seen.at(-1)?.followUpMessage).toContain("decision");
		expect(terminalEvent(result.traceDir).accepted).toBe(true);
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
			expect(seat.agent.systemPrompt).not.toContain(ENVELOPE_CONTRACT_BUNDLED);
			// No fanning out further from inside a panel seat.
			expect(seat.agent.spawns).toBeUndefined();
		}
	});

	it("gives the fuser the envelope contract and leaves its tools alone", async () => {
		const { fuser } = await seatAgents(tempDir("derive-fuser"));
		expect(fuser?.agent.systemPrompt).toContain(ENVELOPE_CONTRACT_BUNDLED);
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

describe("review acceptance decisions", () => {
	it.each([
		{ name: "approval with blockers", report: { ...APPROVED_REVIEW, blocking: ["unsafe migration"] } },
		{ name: "approval with unmet requirements", report: { ...REJECTED_REVIEW, approved: true, blocking: [] } },
		{ name: "rejection without a reason", report: { ...APPROVED_REVIEW, approved: false } },
	])("corrects $name before accepting the review", async ({ report }) => {
		const script = scripted([() => outcome(envelope(report)), () => outcome(envelope(APPROVED_REVIEW))]);
		const result = await runAdw({
			host: host(tempDir("review-correction")),
			workflow: REVIEW_PHASE,
			request: "review migration",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		expect(result.summary.phases[0]?.passed).toBe(true);
		expect(result.summary.phases[0]?.attempts).toBe(2);
		expect(script.seen[1]?.followUpMessage).toContain("approved");
	});

	it("retries an inconsistent fuser report while retaining the panel", async () => {
		const script = scripted([
			() => outcome("Keep the migration reversible."),
			() => outcome("Check every migrated record."),
			() => outcome(envelope({ ...REJECTED_REVIEW, approved: true })),
			() => outcome(envelope(REJECTED_REVIEW)),
		]);
		const result = await runAdw({
			host: host(tempDir("fusion-review-correction")),
			workflow: {
				...FUSION_PHASE,
				acceptance: "review",
				phases: FUSION_PHASE.phases.map(phase => ({ ...phase, gates: [VERDICT_GATE] })),
			},
			request: "review migration",
			seatRunner: script.runner,
		});

		expect(result.summary.phases[0]?.passed).toBe(true);
		expect(result.summary.phases[0]?.attempts).toBe(2);
		expect(result.summary.accepted).toBe(false);
		expect(script.seen.filter(request => request.readOnly)).toHaveLength(2);
		expect(script.seen.at(-1)?.followUpMessage).toContain("approved");
		expect(terminalEvent(result.traceDir).accepted).toBe(false);
	});

	it.each([
		{ name: "rejected", report: REJECTED_REVIEW, accepted: false },
		{ name: "approved", report: APPROVED_REVIEW, accepted: true },
	])(
		"keeps phase success separate from $name review acceptance and isolation landing",
		async ({ name, report, accepted }) => {
			const cwd = tempDir(`review-${name}`);
			await $`git init -q`.cwd(cwd).quiet();
			await $`git config user.email a@b.c`.cwd(cwd).quiet();
			await $`git config user.name t`.cwd(cwd).quiet();
			fs.writeFileSync(path.join(cwd, "tracked.txt"), "original\n");
			await $`git add -A`.cwd(cwd).quiet();
			await $`git commit -qm init`.cwd(cwd).quiet();
			const script = scripted([() => outcome(envelope(report))]);
			const result = await runAdw({
				host: host(cwd),
				workflow: {
					...REVIEW_PHASE,
					isolation: true,
					phases: [
						{ name: "build", kind: "code", command: "printf 'migration change\\n' > tracked.txt" },
						...REVIEW_PHASE.phases,
					],
				},
				request: "review migration",
				seatRunner: script.runner,
			});

			expect(result.summary.accepted).toBe(accepted);
			expect(result.summary.phases.every(phase => phase.passed)).toBe(true);
			expect(script.seen).toHaveLength(1);
			expect(result.isolation?.applied).toBe(accepted);
			expect(fs.readFileSync(path.join(cwd, "tracked.txt"), "utf8")).toBe(
				accepted ? "migration change\n" : "original\n",
			);
			expect(terminalEvent(result.traceDir)).toEqual({ accepted, reason: result.summary.reason });
			if (!accepted) {
				expect(result.summary.reason).toContain("migration loses records");
				expect(fs.readFileSync(result.isolation?.patchPath as string, "utf8")).toContain("migration change");
			}
		},
	);

	it.each(["code", "agent"] as const)("does not let a trailing %s result reuse an earlier approval", async kind => {
		const script = scripted([
			() => outcome(envelope(APPROVED_REVIEW)),
			() => outcome(envelope({ changed_files: ["migration.ts"] })),
		]);
		const result = await runAdw({
			host: host(tempDir(`review-trailing-${kind}`)),
			workflow: {
				...REVIEW_PHASE,
				phases: [
					...REVIEW_PHASE.phases,
					kind === "code"
						? { name: "after", kind: "code", command: "true" }
						: { name: "after", kind: "agent", owner: "sonic" },
				],
			},
			request: "review migration",
			seatRunner: script.runner,
		});

		expect(result.summary.phases.every(phase => phase.passed)).toBe(true);
		expect(result.summary.accepted).toBe(false);
		expect(terminalEvent(result.traceDir).accepted).toBe(false);
	});
});

describe("review revision decisions", () => {
	const workflow: AdwWorkflowConfig = {
		name: "revise",
		maxAttempts: 2,
		acceptance: "review",
		phases: [
			{ name: "build", kind: "agent", owner: "sonic" },
			{
				name: "check",
				kind: "code",
				command: `bun -e 'JSON.parse(await Bun.file("rows.json").text())' && printf 'checked\\n' >> checks.log`,
			},
			{
				name: "review",
				kind: "agent",
				owner: "sonic",
				gates: [VERDICT_GATE],
				onReject: { to: "build", maxRevisions: 1 },
			},
		],
	};

	it("corrects the reviewer before revising the builder and rerunning verification", async () => {
		const cwd = tempDir("review-revise");
		const script = scripted([
			() => {
				fs.writeFileSync(path.join(cwd, "rows.json"), "[1]");
				return outcome(envelope());
			},
			() => outcome(envelope({ ...REJECTED_REVIEW, approved: true })),
			() => outcome(envelope(REJECTED_REVIEW)),
			() => {
				fs.writeFileSync(path.join(cwd, "rows.json"), "[1,7]");
				return outcome(envelope());
			},
			() => outcome(envelope(APPROVED_REVIEW)),
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow,
			request: "preserve every migrated record",
			seatRunner: script.runner,
		});

		expect(script.seen.map(seat => seat.assignment)).toEqual([
			"revise/build",
			"revise/review",
			"revise/review",
			"revise/build",
			"revise/review",
		]);
		expect(script.seen[3]?.task).toContain("migration check lost row 7");
		expect(script.seen[3]?.followUpMessage).toContain("migration loses records");
		expect(script.seen[3]?.id).toBe(script.seen[0]?.id as string);
		expect(script.seen[4]?.followUpMessage).toBe(script.seen[4]?.task);
		expect(fs.readFileSync(path.join(cwd, "checks.log"), "utf8")).toBe("checked\nchecked\n");
		expect(result.summary.accepted).toBe(true);
		expect(result.summary.phases.filter(phase => phase.invalidated).map(phase => phase.name)).toEqual([
			"build",
			"check",
			"review",
		]);
		expect(result.summary.phases.filter(phase => !phase.invalidated).map(phase => phase.name)).toEqual([
			"build",
			"check",
			"review",
		]);
		expect(terminalEvent(result.traceDir).accepted).toBe(true);
	});

	it("refreshes fusion opinions after revision but not after a fuser format retry", async () => {
		const cwd = tempDir("review-refresh-panel");
		let builds = 0;
		let reviews = 0;
		const observations: number[] = [];
		const seen: SeatRequest[] = [];
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				...workflow,
				phases: [
					...workflow.phases.slice(0, 2),
					{
						name: "review",
						kind: "fusion",
						panel: [{ owner: "sonic" }, { owner: "sonic" }],
						fuser: { owner: "sonic" },
						gates: [VERDICT_GATE],
						onReject: { to: "build", maxRevisions: 1 },
					},
				],
			},
			request: "preserve every migrated record",
			seatRunner: async seat => {
				seen.push(seat);
				if (seat.assignment === "revise/build") {
					builds++;
					fs.writeFileSync(path.join(cwd, "rows.json"), builds === 1 ? "[1]" : "[1,7]");
					return outcome(envelope());
				}
				if (seat.readOnly) {
					const rows: number[] = JSON.parse(fs.readFileSync(path.join(cwd, "rows.json"), "utf8"));
					observations.push(rows.length);
					return outcome(`The current migration contains ${rows.length} rows.`);
				}
				reviews++;
				if (reviews === 1) return outcome(envelope({ ...REJECTED_REVIEW, approved: true }));
				return outcome(envelope(builds === 1 ? REJECTED_REVIEW : APPROVED_REVIEW));
			},
		});

		expect(result.summary.accepted).toBe(true);
		expect(observations).toEqual([1, 1, 2, 2]);
		expect(seen.filter(seat => !seat.readOnly && seat.assignment === "revise/review")).toHaveLength(3);
		expect(seen.at(-1)?.task).toContain("current migration contains 2 rows");
		expect(fs.readFileSync(path.join(cwd, "checks.log"), "utf8")).toBe("checked\nchecked\n");
	});

	it("refuses revised code that fails a previously passing check without asking for approval", async () => {
		const cwd = tempDir("review-revised-failure");
		const script = scripted([
			() => {
				fs.writeFileSync(path.join(cwd, "rows.json"), "[1]");
				return outcome(envelope());
			},
			() => outcome(envelope(REJECTED_REVIEW)),
			() => {
				fs.writeFileSync(path.join(cwd, "rows.json"), "[1,7");
				return outcome(envelope());
			},
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow: { ...workflow, maxAttempts: 1 },
			request: "preserve every migrated record",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(false);
		expect(script.seen.map(seat => seat.assignment)).toEqual(["revise/build", "revise/review", "revise/build"]);
		expect(
			result.summary.phases
				.filter(phase => phase.name === "check")
				.map(phase => ({
					passed: phase.passed,
					invalidated: phase.invalidated,
				})),
		).toEqual([
			{ passed: true, invalidated: true },
			{ passed: false, invalidated: false },
		]);
		expect(result.summary.phases.at(-1)?.violations.join(" ")).toContain("JSON");
		expect(terminalEvent(result.traceDir).accepted).toBe(false);
	});

	it("honors a revision budget beyond the old driver step cap and terminates with the review reason", async () => {
		const cwd = tempDir("review-revision-budget");
		let builds = 0;
		let reviews = 0;
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				...workflow,
				maxAttempts: 1,
				phases: workflow.phases.map(phase =>
					phase.onReject ? { ...phase, onReject: { to: "build", maxRevisions: 12 } } : phase,
				),
			},
			request: "preserve every migrated record",
			seatRunner: async seat => {
				if (seat.assignment === "revise/build") {
					builds++;
					fs.writeFileSync(path.join(cwd, "rows.json"), "[1]");
					return outcome(envelope());
				}
				reviews++;
				return outcome(envelope(REJECTED_REVIEW));
			},
		});

		expect(builds).toBe(13);
		expect(reviews).toBe(13);
		expect(result.summary.accepted).toBe(false);
		expect(result.summary.reason).toContain("migration loses records");
		expect(result.summary.phases.at(-1)?.passed).toBe(true);
		expect(terminalEvent(result.traceDir)).toEqual({ accepted: false, reason: result.summary.reason });
	}, 20_000);

	it("resumes with the rejected report and cannot replenish an already consumed revision", async () => {
		const cwd = tempDir("review-resume-revision");
		const bounded = { ...workflow, maxAttempts: 1 };
		const first = scripted([
			() => {
				fs.writeFileSync(path.join(cwd, "rows.json"), "[1]");
				return outcome(envelope());
			},
			() => outcome(envelope(REJECTED_REVIEW)),
		]);
		const interrupted = await runAdw({
			host: host(cwd),
			workflow: bounded,
			request: "preserve every migrated record",
			seatRunner: first.runner,
			onPhase: progress => {
				if (progress.phase === "review" && progress.outcome === "retry")
					throw new Error("interrupted after revision");
			},
		}).catch((err: unknown) => err);
		expect(interrupted).toBeInstanceOf(AdwRunError);
		const second = scripted([() => outcome(envelope()), () => outcome(envelope(REJECTED_REVIEW))]);
		const resumed = await runAdw({
			host: host(cwd),
			workflow: bounded,
			request: "",
			resumeAdwId: (interrupted as AdwRunError).adwId,
			seatRunner: second.runner,
		});

		expect(second.seen.map(seat => seat.assignment)).toEqual(["revise/build", "revise/review"]);
		expect(second.seen[0]?.task).toContain("migration check lost row 7");
		expect(second.seen[0]?.followUpMessage).toContain("migration loses records");
		expect(resumed.summary.accepted).toBe(false);
		expect(resumed.summary.reason).toContain("migration loses records");
		expect(terminalEvent(resumed.traceDir)).toEqual({ accepted: false, reason: resumed.summary.reason });
	});
});

describe("input decisions", () => {
	it("delivers the declared plan to build and to the code check while the check cannot clobber it", async () => {
		const cwd = tempDir("inputs-plan");
		const script = scripted([
			() =>
				outcome(
					envelope({
						notes_for_next_agent: "read plan.md first",
						rows: 7,
					}),
				),
			() => outcome(envelope()),
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "planned",
				maxAttempts: 2,
				phases: [
					{ name: "plan", kind: "agent", owner: "sonic" },
					{
						name: "check",
						kind: "code",
						inputs: ["plan"],
						command: `bun -e 'const j = JSON.parse(await Bun.file(process.env.ADW_INPUTS).text()); if (j.plan.payload.rows !== 7 || j.plan.version !== 1) process.exit(1);'`,
					},
					{ name: "build", kind: "agent", owner: "sonic", inputs: ["plan"] },
				],
			},
			request: "build from the plan",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		// The build consumes the plan's output, not the check's incidental result.
		expect(script.seen[1]?.task).toContain("read plan.md first");
		expect(script.seen[1]?.task).not.toContain("exited 0");
	});

	it("hands a join both named predecessor outputs regardless of declaration order", async () => {
		const cwd = tempDir("inputs-join");
		const script = scripted([
			() => outcome(envelope({ summary: "measured latency", notes_for_next_agent: "p99 is 40ms" })),
			() => outcome(envelope({ summary: "measured throughput", notes_for_next_agent: "40k rps sustained" })),
			() => outcome(envelope()),
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "joined",
				maxAttempts: 1,
				phases: [
					{ name: "latency", kind: "agent", owner: "sonic" },
					{ name: "throughput", kind: "agent", owner: "sonic" },
					{ name: "join", kind: "agent", owner: "sonic", inputs: ["throughput", "latency"] },
				],
			},
			request: "summarize both measurements",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		const task = script.seen[2]?.task ?? "";
		expect(task).toContain("p99 is 40ms");
		expect(task).toContain("40k rps sustained");
	});

	it("reselects the revised producer version after a review revision", async () => {
		const cwd = tempDir("inputs-revision");
		const script = scripted([
			() => outcome(envelope({ summary: "wrote v1" })),
			() => outcome(envelope(REJECTED_REVIEW)),
			() => outcome(envelope({ summary: "wrote v2" })),
			() => outcome(envelope(APPROVED_REVIEW)),
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "reselect",
				maxAttempts: 1,
				acceptance: "review",
				phases: [
					{ name: "build", kind: "agent", owner: "sonic" },
					{
						name: "review",
						kind: "agent",
						owner: "sonic",
						inputs: ["build"],
						gates: [VERDICT_GATE],
						onReject: { to: "build", maxRevisions: 1 },
					},
				],
			},
			request: "review the build",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		expect(script.seen[1]?.task).toContain("wrote v1");
		expect(script.seen[3]?.task).toContain("wrote v2");
		expect(script.seen[3]?.task).not.toContain("wrote v1");
	});

	const DURABLE: AdwWorkflowConfig = {
		name: "durable",
		maxAttempts: 1,
		phases: [
			{ name: "plan", kind: "agent", owner: "sonic" },
			{ name: "build", kind: "agent", owner: "sonic", inputs: ["plan"] },
		],
	};

	/** Crash mid-build after the plan was accepted, returning the dead run. */
	async function crashedDurableRun(cwd: string): Promise<AdwRunError> {
		const first = scripted([
			() => outcome(envelope({ notes_for_next_agent: "the durable plan" })),
			() => {
				throw new Error("process died mid-build");
			},
		]);
		const failed = await runAdw({
			host: host(cwd),
			workflow: DURABLE,
			request: "durable inputs",
			seatRunner: first.runner,
		}).catch((err: unknown) => err);
		expect(failed).toBeInstanceOf(AdwRunError);
		return failed as AdwRunError;
	}

	it("resumes with the same selected input", async () => {
		const cwd = tempDir("inputs-resume");
		const dead = await crashedDurableRun(cwd);

		const second = scripted([() => outcome(envelope())]);
		const resumed = await runAdw({
			host: host(cwd),
			workflow: DURABLE,
			request: "",
			resumeAdwId: dead.adwId,
			seatRunner: second.runner,
		});
		expect(resumed.summary.accepted).toBe(true);
		expect(second.seen[0]?.task).toContain("the durable plan");
	});

	it("refuses to resume over a corrupted authoritative envelope", async () => {
		const cwd = tempDir("inputs-corrupt");
		const dead = await crashedDurableRun(cwd);

		// The trace says plan v1 was accepted; the store no longer backs it. The
		// resume must refuse rather than hand the consumer invented context.
		fs.writeFileSync(path.join(dead.traceDir, "envelopes", "plan.1.json"), "{broken");
		const refused = await runAdw({
			host: host(cwd),
			workflow: DURABLE,
			request: "",
			resumeAdwId: dead.adwId,
			seatRunner: scripted([() => outcome(envelope())]).runner,
		}).catch((err: unknown) => err);
		expect(refused).toBeInstanceOf(AdwRunError);
		expect((refused as Error).message).toContain("plan");
	});
});

/** A git repo with committed files, so isolation and write guards have a real baseline. */
async function repo(cwd: string, files: Record<string, string>): Promise<void> {
	await $`git init -q`.cwd(cwd).quiet();
	await $`git config user.email a@b.c`.cwd(cwd).quiet();
	await $`git config user.name t`.cwd(cwd).quiet();
	for (const [name, content] of Object.entries(files)) {
		fs.mkdirSync(path.dirname(path.join(cwd, name)), { recursive: true });
		fs.writeFileSync(path.join(cwd, name), content);
	}
	await $`git add -A`.cwd(cwd).quiet();
	await $`git commit -qm init`.cwd(cwd).quiet();
}

describe("write scope decisions", () => {
	it("reverts a same-size out-of-scope rewrite and accepts the corrected attempt", async () => {
		const cwd = tempDir("scope-revert");
		await repo(cwd, { "impl.ts": "original\n" });
		const script = scripted([
			() => {
				fs.writeFileSync(path.join(cwd, "PLAN.md"), "the plan");
				fs.writeFileSync(path.join(cwd, "impl.ts"), "changed!\n");
				return outcome(envelope({ artifacts: ["PLAN.md"] }));
			},
			() => outcome(envelope({ artifacts: ["PLAN.md"] })),
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "scoped",
				maxAttempts: 2,
				phases: [{ name: "plan", kind: "agent", owner: "sonic", writes: ["PLAN.md"], gates: ["artifacts_exist"] }],
			},
			request: "plan only",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		expect(result.summary.phases[0]?.attempts).toBe(2);
		expect(script.seen[1]?.followUpMessage).toContain("impl.ts");
		expect(script.seen[1]?.followUpMessage).toContain("write scope");
		expect(fs.readFileSync(path.join(cwd, "impl.ts"), "utf8")).toBe("original\n");
		expect(fs.readFileSync(path.join(cwd, "PLAN.md"), "utf8")).toBe("the plan");
	});

	it("protection beats a declared write and untouched user dirt survives byte-for-byte", async () => {
		const cwd = tempDir("scope-protected");
		await repo(cwd, { "eval.sh": "exit 0\n" });
		fs.writeFileSync(path.join(cwd, "notes.txt"), "mine\n");
		fs.mkdirSync(path.join(cwd, ".omp/adw"), { recursive: true });
		const script = scripted([
			() => {
				fs.writeFileSync(path.join(cwd, "eval.sh"), "exit 1\n");
				fs.writeFileSync(path.join(cwd, ".omp/adw/evil.yml"), "name: evil");
				fs.writeFileSync(path.join(cwd, "out.md"), "report");
				return outcome(envelope({ artifacts: ["out.md"] }));
			},
			() => outcome(envelope({ artifacts: ["out.md"] })),
		]);
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "sealed",
				maxAttempts: 2,
				protected: ["eval.sh"],
				phases: [
					{
						name: "review",
						kind: "agent",
						owner: "sonic",
						writes: ["eval.sh", "out.md"],
						gates: ["artifacts_exist"],
					},
				],
			},
			request: "report without touching the evaluator",
			seatRunner: script.runner,
		});

		expect(result.summary.accepted).toBe(true);
		const correction = script.seen[1]?.followUpMessage ?? "";
		expect(correction).toContain("eval.sh");
		expect(correction).toContain(".omp/adw/evil.yml");
		// Declaring the forbidden path did not authorize it.
		expect(fs.readFileSync(path.join(cwd, "eval.sh"), "utf8")).toBe("exit 0\n");
		expect(fs.existsSync(path.join(cwd, ".omp/adw/evil.yml"))).toBe(false);
		// Authorized work and the user's own dirt both survive.
		expect(fs.readFileSync(path.join(cwd, "out.md"), "utf8")).toBe("report");
		expect(fs.readFileSync(path.join(cwd, "notes.txt"), "utf8")).toBe("mine\n");
	});

	it("restores an unauthorized creation when the writer crashes", async () => {
		const cwd = tempDir("scope-crash");
		await repo(cwd, { "impl.ts": "original\n" });
		const failed = await runAdw({
			host: host(cwd),
			workflow: {
				name: "scoped",
				maxAttempts: 1,
				phases: [{ name: "plan", kind: "agent", owner: "sonic", writes: ["PLAN.md"] }],
			},
			request: "plan only",
			seatRunner: scripted([
				() => {
					fs.writeFileSync(path.join(cwd, "secret.txt"), "exfiltrated");
					throw new Error("provider died mid-turn");
				},
			]).runner,
		}).catch((err: unknown) => err);

		expect(failed).toBeInstanceOf(AdwRunError);
		expect(fs.existsSync(path.join(cwd, "secret.txt"))).toBe(false);
	});

	it("fully reverts a directory squatting a guarded file and rejects the attempt", async () => {
		const cwd = tempDir("scope-squat");
		await repo(cwd, { "cfg.txt": "keep\n" });
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "scoped",
				maxAttempts: 1,
				phases: [{ name: "plan", kind: "agent", owner: "sonic", writes: ["PLAN.md"] }],
			},
			request: "plan only",
			seatRunner: scripted([
				() => {
					// Replace a guarded file with a directory: the deletion and the
					// squatting creation must both be reverted, in that order.
					fs.rmSync(path.join(cwd, "cfg.txt"));
					fs.mkdirSync(path.join(cwd, "cfg.txt"));
					fs.writeFileSync(path.join(cwd, "cfg.txt/child.txt"), "squatting");
					return outcome(envelope());
				},
			]).runner,
		});

		expect(result.summary.accepted).toBe(false);
		expect(result.summary.reason).toContain("cfg.txt");
		expect(result.summary.reason).toContain("write scope");
		expect(fs.statSync(path.join(cwd, "cfg.txt")).isFile()).toBe(true);
		expect(fs.readFileSync(path.join(cwd, "cfg.txt"), "utf8")).toBe("keep\n");
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

	it("rechecks the persisted final review after interruption before finish without asking the reviewer again", async () => {
		const cwd = tempDir("resume-refused-review");
		const first = scripted([() => outcome(envelope(REJECTED_REVIEW))]);
		const failed = await runAdw({
			host: host(cwd),
			workflow: REVIEW_PHASE,
			request: "review migration",
			seatRunner: first.runner,
			onPhase: progress => {
				if (progress.outcome === "advanced") throw new Error("interrupted after final review");
			},
		}).catch((err: unknown) => err);
		expect(failed).toBeInstanceOf(AdwRunError);

		const second = scripted([() => outcome(envelope(APPROVED_REVIEW))]);
		const resumed = await runAdw({
			host: host(cwd),
			workflow: REVIEW_PHASE,
			request: "",
			resumeAdwId: (failed as AdwRunError).adwId,
			seatRunner: second.runner,
		});

		expect(second.seen).toHaveLength(0);
		expect(resumed.summary.phases[0]?.passed).toBe(true);
		expect(resumed.summary.phases[0]?.attempts).toBe(1);
		expect(resumed.summary.accepted).toBe(false);
		expect(resumed.summary.reason).toContain("migration loses records");
		expect(terminalEvent(resumed.traceDir)).toEqual({ accepted: false, reason: resumed.summary.reason });
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
		expect((error as Error).message).toContain("request.txt does not exist");
	});

	it("refuses to reattach when the sandbox is gone and nothing was settled", async () => {
		const cwd = tempDir("resume-iso-gone");
		await $`git init -q`.cwd(cwd).quiet();
		await $`git config user.email a@b.c`.cwd(cwd).quiet();
		await $`git config user.name t`.cwd(cwd).quiet();
		fs.writeFileSync(path.join(cwd, "a.txt"), "a\n");
		await $`git add -A`.cwd(cwd).quiet();
		await $`git commit -qm init`.cwd(cwd).quiet();
		const failed = await runAdw({
			host: host(cwd),
			workflow: { ...TWO_PHASE, isolation: true },
			request: "ship it",
			seatRunner: scripted([
				() => {
					throw new Error("process died mid-plan");
				},
			]).runner,
		}).catch((err: unknown) => err);
		expect(failed).toBeInstanceOf(AdwRunError);

		// Simulate raw process death: no settled delivery, no reattachable state.
		const runDir = path.dirname((failed as AdwRunError).traceDir);
		fs.rmSync(path.join(runDir, "delivery.json"), { force: true });
		fs.rmSync(path.join(runDir, "isolation.json"), { force: true });
		const refused = await runAdw({
			host: host(cwd),
			workflow: { ...TWO_PHASE, isolation: true },
			request: "",
			resumeAdwId: (failed as AdwRunError).adwId,
			seatRunner: scripted([() => outcome(envelope())]).runner,
		}).catch((err: unknown) => err);
		expect((refused as Error).message).toContain("is gone");
	});

	it("restores an in-place rejection's correction across resume", async () => {
		const cwd = tempDir("resume-correction");
		// Attempt 1 claims a file it never wrote; the process dies before the
		// retry can run. The resumed retry must still know WHAT was wrong.
		const first = scripted([
			() => outcome(envelope({ artifacts: ["never.md"] })),
			() => {
				throw new Error("process died before the retry");
			},
		]);
		const failed = await runAdw({
			host: host(cwd),
			workflow: { ...AGENT_PHASE, maxAttempts: 3 },
			request: "make it",
			seatRunner: first.runner,
		}).catch((err: unknown) => err);
		expect(failed).toBeInstanceOf(AdwRunError);

		const second = scripted([
			() => {
				fs.writeFileSync(path.join(cwd, "out.md"), "content");
				return outcome(envelope({ artifacts: ["out.md"] }));
			},
		]);
		const resumed = await runAdw({
			host: host(cwd),
			workflow: { ...AGENT_PHASE, maxAttempts: 3 },
			request: "",
			resumeAdwId: (failed as AdwRunError).adwId,
			seatRunner: second.runner,
		});
		expect(resumed.summary.accepted).toBe(true);
		// The diagnostic survived the crash, not just the attempt count.
		expect(second.seen[0]?.followUpMessage ?? second.seen[0]?.task).toContain("never.md");
	});

	it("refuses to resume against a changed workflow definition", async () => {
		const cwd = tempDir("resume-drift");
		const failed = await runAdw({
			host: host(cwd),
			workflow: TWO_PHASE,
			request: "ship it",
			seatRunner: scripted([
				() => {
					throw new Error("process died mid-plan");
				},
			]).runner,
		}).catch((err: unknown) => err);
		expect(failed).toBeInstanceOf(AdwRunError);

		const drifted = {
			...TWO_PHASE,
			phases: [TWO_PHASE.phases[0]!, { ...TWO_PHASE.phases[1]!, prompt: "redesigned" }],
		};
		const refused = await runAdw({
			host: host(cwd),
			workflow: drifted,
			request: "",
			resumeAdwId: (failed as AdwRunError).adwId,
			seatRunner: scripted([() => outcome(envelope())]).runner,
		}).catch((err: unknown) => err);
		expect((refused as Error).message).toContain("changed since run");
	});

	it("records the delivery once and refuses both re-settling and re-resuming", async () => {
		const cwd = tempDir("resume-delivered");
		await $`git init -q`.cwd(cwd).quiet();
		await $`git config user.email a@b.c`.cwd(cwd).quiet();
		await $`git config user.name t`.cwd(cwd).quiet();
		fs.writeFileSync(path.join(cwd, "tracked.txt"), "original\n");
		await $`git add -A`.cwd(cwd).quiet();
		await $`git commit -qm init`.cwd(cwd).quiet();
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				...REVIEW_PHASE,
				isolation: true,
				phases: [
					{ name: "build", kind: "code", command: "printf 'delivered\\n' > tracked.txt" },
					...REVIEW_PHASE.phases,
				],
			},
			request: "review migration",
			seatRunner: scripted([() => outcome(envelope(APPROVED_REVIEW))]).runner,
		});
		expect(result.isolation?.applied).toBe(true);
		expect(fs.readFileSync(path.join(cwd, "tracked.txt"), "utf8")).toBe("delivered\n");

		// The sandbox is long gone; the recorded outcome must answer instead of
		// a second application. A revert makes a re-apply visible if it happened.
		fs.writeFileSync(path.join(cwd, "tracked.txt"), "reverted\n");
		const runDir = path.dirname(result.traceDir);
		const again = await settleIsolation(
			{
				handle: { mergedDir: "/nonexistent", backend: 0, fellBack: false, fallbackReason: null },
				context: {
					repoRoot: cwd,
					baseline: {
						root: { repoRoot: cwd, headCommit: "", staged: "", unstaged: "", untracked: [], untrackedPatch: "" },
						nested: [],
					},
				},
			},
			true,
			runDir,
			result.adwId,
		);
		expect(again.applied).toBe(true);
		expect(fs.readFileSync(path.join(cwd, "tracked.txt"), "utf8")).toBe("reverted\n");

		const refused = await runAdw({
			host: host(cwd),
			workflow: {
				...REVIEW_PHASE,
				isolation: true,
				phases: [
					{ name: "build", kind: "code", command: "printf 'delivered\\n' > tracked.txt" },
					...REVIEW_PHASE.phases,
				],
			},
			request: "",
			resumeAdwId: result.adwId,
			seatRunner: scripted([() => outcome(envelope(APPROVED_REVIEW))]).runner,
		}).catch((err: unknown) => err);
		expect((refused as Error).message).toContain("already settled its delivery");
	});
});

describe("concurrent workflow decisions", () => {
	it("overlaps isolated writers and gives the join both accepted inputs and combined files", async () => {
		const cwd = tempDir("dag-join");
		await repo(cwd, { "left.txt": "original left\n", "right.txt": "original right\n" });
		const bothStarted = Promise.withResolvers<void>();
		const writerRoots = new Map<string, string>();
		let active = 0;
		let peak = 0;
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "dag",
				isolation: true,
				concurrency: 2,
				maxAttempts: 1,
				phases: [
					{ name: "left", kind: "agent", owner: "sonic", dependsOn: [], writes: ["left.txt"] },
					{ name: "right", kind: "agent", owner: "sonic", dependsOn: [], writes: ["right.txt"] },
					{
						name: "check",
						kind: "code",
						dependsOn: ["left", "right"],
						inputs: ["right", "left"],
						command:
							`bun -e 'const j = await Bun.file(process.env.ADW_INPUTS).json(); ` +
							`if (j.left.payload.side !== "left" || j.right.payload.side !== "right" || ` +
							`await Bun.file("left.txt").text() !== "accepted left\\n" || ` +
							`await Bun.file("right.txt").text() !== "accepted right\\n") process.exit(1)'`,
					},
					{
						name: "join",
						kind: "agent",
						owner: "sonic",
						inputs: ["right", "left"],
						dependsOn: ["check"],
						writes: [],
					},
				],
			},
			request: "combine independently accepted work",
			onPhase: progress => {
				if (progress.phase === "check" && !progress.outcome) expect(active).toBe(0);
			},
			seatRunner: async seat => {
				const name = seat.assignment.split("/")[1]!;
				expect(seat.root).toBeDefined();
				const root = seat.root!;
				expect(root).not.toBe(cwd);
				if (name === "join") {
					expect(active).toBe(0);
					expect(await Bun.file(path.join(root, "left.txt")).text()).toBe("accepted left\n");
					expect(await Bun.file(path.join(root, "right.txt")).text()).toBe("accepted right\n");
					expect(seat.task).toContain("left handoff");
					expect(seat.task).toContain("right handoff");
					return outcome(envelope());
				}
				writerRoots.set(name, root);
				active++;
				peak = Math.max(peak, active);
				await Bun.write(path.join(root, `${name}.txt`), `accepted ${name}\n`);
				if (active === 2) bothStarted.resolve();
				await bothStarted.promise;
				const sibling = name === "left" ? "right" : "left";
				expect(await Bun.file(path.join(root, `${sibling}.txt`)).text()).toBe(`original ${sibling}\n`);
				expect(await Bun.file(path.join(cwd, `${name}.txt`)).text()).toBe(`original ${name}\n`);
				active--;
				return outcome(envelope({ side: name, notes_for_next_agent: `${name} handoff` }));
			},
		});
		roots.push(path.dirname(result.traceDir));

		expect(peak).toBe(2);
		expect(writerRoots.get("left")).not.toBe(writerRoots.get("right"));
		expect(result.summary.accepted).toBe(true);
		expect(result.isolation?.applied).toBe(true);
		expect(await Bun.file(path.join(cwd, "left.txt")).text()).toBe("accepted left\n");
		expect(await Bun.file(path.join(cwd, "right.txt")).text()).toBe("accepted right\n");
	}, 30_000);

	it("keeps a schema-rejected patch out of a sibling workspace and the eventual join", async () => {
		const cwd = tempDir("dag-schema");
		await repo(cwd, { "value.txt": "original\n" });
		const siblingStarted = Promise.withResolvers<void>();
		const rejected = Promise.withResolvers<void>();
		const probeAccepted = Promise.withResolvers<void>();
		let attempts = 0;
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "dag",
				isolation: true,
				concurrency: 2,
				maxAttempts: 2,
				phases: [
					{
						name: "write",
						kind: "agent",
						owner: "sonic",
						dependsOn: [],
						writes: ["value.txt"],
						schema: { type: "object", required: ["valid"], properties: { valid: { const: true } } },
					},
					{ name: "sibling", kind: "agent", owner: "sonic", dependsOn: [], writes: [] },
					{ name: "probe", kind: "agent", owner: "sonic", dependsOn: ["sibling"], writes: [] },
					{
						name: "join",
						kind: "agent",
						owner: "sonic",
						dependsOn: ["write", "probe"],
						inputs: ["write", "sibling"],
						writes: [],
					},
				],
			},
			request: "never publish rejected edits",
			onPhase: progress => {
				if (progress.phase === "write" && progress.outcome === "retry") rejected.resolve();
				if (progress.phase === "probe" && progress.outcome === "advanced") probeAccepted.resolve();
			},
			seatRunner: async seat => {
				const file = path.join(seat.root!, "value.txt");
				if (seat.assignment === "dag/write") {
					attempts++;
					if (attempts === 1) {
						await siblingStarted.promise;
						await Bun.write(file, "rejected poison\n");
						return outcome(envelope({ valid: false, notes_for_next_agent: "poison handoff" }));
					}
					await probeAccepted.promise;
					await Bun.write(file, "accepted replacement\n");
					return outcome(envelope({ valid: true, notes_for_next_agent: "clean handoff" }));
				}
				if (seat.assignment === "dag/sibling") {
					siblingStarted.resolve();
					await rejected.promise;
					expect(await Bun.file(file).text()).toBe("original\n");
					return outcome(envelope());
				}
				if (seat.assignment === "dag/probe") {
					// Unlike the already-running sibling, this workspace was cloned
					// after rejection. Integrating before schema checks poisons it.
					expect(await Bun.file(file).text()).toBe("original\n");
					return outcome(envelope());
				}
				expect(await Bun.file(file).text()).toBe("accepted replacement\n");
				expect(seat.task).toContain("clean handoff");
				expect(seat.task).not.toContain("poison handoff");
				return outcome(envelope());
			},
		});
		roots.push(path.dirname(result.traceDir));

		expect(attempts).toBe(2);
		expect(result.summary.accepted).toBe(true);
		expect(await Bun.file(path.join(cwd, "value.txt")).text()).toBe("accepted replacement\n");
	}, 30_000);

	it("refuses delivery for conflicting sibling patches and preserves both edits as evidence", async () => {
		const cwd = tempDir("dag-conflict");
		await repo(cwd, { "shared.txt": "original\n" });
		const bothStarted = Promise.withResolvers<void>();
		const firstAccepted = Promise.withResolvers<void>();
		let started = 0;
		const seen: string[] = [];
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "dag",
				isolation: true,
				concurrency: 2,
				maxAttempts: 1,
				phases: [
					{ name: "left", kind: "agent", owner: "sonic", dependsOn: [], writes: ["shared.txt"] },
					{ name: "right", kind: "agent", owner: "sonic", dependsOn: [], writes: ["shared.txt"] },
					{ name: "join", kind: "agent", owner: "sonic", dependsOn: ["left", "right"], writes: [] },
				],
			},
			request: "refuse incompatible edits",
			onPhase: progress => {
				if (progress.phase === "left" && progress.outcome === "advanced") firstAccepted.resolve();
			},
			seatRunner: async seat => {
				seen.push(seat.assignment);
				const side = seat.assignment.split("/")[1]!;
				await Bun.write(path.join(seat.root!, "shared.txt"), `${side} edit\n`);
				if (++started === 2) bothStarted.resolve();
				await bothStarted.promise;
				if (side === "right") await firstAccepted.promise;
				return outcome(envelope());
			},
		});
		const runDir = path.dirname(result.traceDir);
		roots.push(runDir);

		expect(result.summary.accepted).toBe(false);
		expect(result.isolation?.applied).toBe(false);
		expect(terminalEvent(result.traceDir).accepted).toBe(false);
		expect(seen).not.toContain("dag/join");
		expect(await Bun.file(path.join(cwd, "shared.txt")).text()).toBe("original\n");
		const patchNames = await fs.promises.readdir(runDir, { recursive: true });
		const patches = await Promise.all(
			patchNames.filter(name => name.endsWith(".patch")).map(name => Bun.file(path.join(runDir, name)).text()),
		);
		expect(patches.join("\n")).toContain("+left edit");
		expect(patches.join("\n")).toContain("+right edit");
	}, 30_000);

	it("blocks an exhausted dependency's descendants without dropping an unrelated late completion", async () => {
		const cwd = tempDir("dag-exhausted");
		await repo(cwd, { "evidence.txt": "original\n" });
		const unrelatedStarted = Promise.withResolvers<void>();
		const exhausted = Promise.withResolvers<void>();
		const seen: string[] = [];
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "dag",
				isolation: true,
				concurrency: 2,
				maxAttempts: 1,
				phases: [
					{ name: "fail", kind: "agent", owner: "sonic", dependsOn: [], writes: [] },
					{ name: "unrelated", kind: "agent", owner: "sonic", dependsOn: [], writes: ["evidence.txt"] },
					{ name: "blocked", kind: "agent", owner: "sonic", dependsOn: ["fail"], writes: [] },
				],
			},
			request: "retain unrelated evidence after failure",
			onPhase: progress => {
				if (progress.phase === "fail" && progress.outcome === "aborted") exhausted.resolve();
			},
			seatRunner: async seat => {
				seen.push(seat.assignment);
				if (seat.assignment === "dag/fail") {
					await unrelatedStarted.promise;
					return outcome("", { exitCode: 1, stderr: "dependency failed" });
				}
				unrelatedStarted.resolve();
				await exhausted.promise;
				await Bun.write(path.join(seat.root!, "evidence.txt"), "unrelated evidence\n");
				return outcome(envelope({ notes_for_next_agent: "unrelated completed" }));
			},
		});
		roots.push(path.dirname(result.traceDir));

		expect(result.summary.accepted).toBe(false);
		expect(seen).not.toContain("dag/blocked");
		expect(result.summary.phases.find(phase => phase.name === "unrelated")?.passed).toBe(true);
		expect(await Bun.file(result.isolation!.patchPath!).text()).toContain("+unrelated evidence");
		expect(await Bun.file(path.join(cwd, "evidence.txt")).text()).toBe("original\n");
	}, 30_000);

	it("drains an invalidated late writer before revision and never publishes its stale output", async () => {
		const cwd = tempDir("dag-revision");
		await repo(cwd, { "build.txt": "original\n", "derived.txt": "original\n" });
		const lateStarted = Promise.withResolvers<void>();
		const invalidated = Promise.withResolvers<void>();
		let lateSettled = false;
		let builds = 0;
		let derivatives = 0;
		let reviews = 0;
		const result = await runAdw({
			host: host(cwd),
			workflow: {
				name: "dag",
				isolation: true,
				concurrency: 2,
				maxAttempts: 1,
				phases: [
					{ name: "build", kind: "agent", owner: "sonic", dependsOn: [], writes: ["build.txt"] },
					{ name: "derived", kind: "agent", owner: "sonic", dependsOn: ["build"], writes: ["derived.txt"] },
					{
						name: "review",
						kind: "agent",
						owner: "sonic",
						dependsOn: ["build"],
						writes: [],
						gates: [VERDICT_GATE],
						onReject: { to: "build", maxRevisions: 1 },
					},
					{
						name: "join",
						kind: "agent",
						owner: "sonic",
						dependsOn: ["derived", "review"],
						inputs: ["derived"],
						writes: [],
					},
				],
			},
			request: "reject stale work after revising its input",
			seatRunner: async seat => {
				if (seat.assignment === "dag/build") {
					if (++builds === 2) expect(lateSettled).toBe(true);
					await Bun.write(path.join(seat.root!, "build.txt"), `build ${builds}\n`);
					return outcome(envelope());
				}
				if (seat.assignment === "dag/derived") {
					if (++derivatives === 1) {
						expect(seat.signal).toBeDefined();
						if (seat.signal!.aborted) invalidated.resolve();
						else seat.signal!.addEventListener("abort", () => invalidated.resolve(), { once: true });
						lateStarted.resolve();
						await invalidated.promise;
						// A provider may finish after cancellation. The old workspace must
						// remain live until this flight drains, but never be integrated.
						await Bun.write(path.join(seat.root!, "derived.txt"), "stale derivative\n");
						lateSettled = true;
						return outcome(envelope({ notes_for_next_agent: "stale handoff" }));
					}
					expect(await Bun.file(path.join(seat.root!, "build.txt")).text()).toBe("build 2\n");
					await Bun.write(path.join(seat.root!, "derived.txt"), "current derivative\n");
					return outcome(envelope({ notes_for_next_agent: "current handoff" }));
				}
				if (seat.assignment === "dag/review") {
					await lateStarted.promise;
					return outcome(envelope(++reviews === 1 ? REJECTED_REVIEW : APPROVED_REVIEW));
				}
				expect(await Bun.file(path.join(seat.root!, "derived.txt")).text()).toBe("current derivative\n");
				expect(seat.task).toContain("current handoff");
				expect(seat.task).not.toContain("stale handoff");
				return outcome(envelope());
			},
		});
		roots.push(path.dirname(result.traceDir));

		expect(result.summary.accepted).toBe(true);
		expect([builds, derivatives, reviews]).toEqual([2, 2, 2]);
		expect(result.summary.phases.filter(phase => phase.name === "derived" && !phase.invalidated)).toHaveLength(1);
		expect(await Bun.file(path.join(cwd, "build.txt")).text()).toBe("build 2\n");
		expect(await Bun.file(path.join(cwd, "derived.txt")).text()).toBe("current derivative\n");
	}, 30_000);

	it("resumes the accepted combined tree without redoing completed work or exceeding the worker bound", async () => {
		const cwd = tempDir("dag-resume");
		await repo(cwd, { "seed.txt": "original\n" });
		const workflow: AdwWorkflowConfig = {
			name: "dag",
			isolation: true,
			concurrency: 2,
			maxAttempts: 2,
			phases: [
				{ name: "seed", kind: "agent", owner: "sonic", dependsOn: [], writes: ["seed.txt"] },
				...["a", "b", "c"].map(name => ({
					name,
					kind: "agent" as const,
					owner: "sonic",
					dependsOn: ["seed"],
					inputs: ["seed"],
					writes: [`${name}.txt`],
				})),
				{ name: "join", kind: "agent", owner: "sonic", dependsOn: ["a", "b", "c"], writes: [] },
			],
		};
		const firstSeen: string[] = [];
		const failed = await runAdw({
			host: host(cwd),
			workflow,
			request: "resume bounded writers",
			seatRunner: async seat => {
				firstSeen.push(seat.assignment);
				await Bun.write(path.join(seat.root!, "seed.txt"), "accepted seed\n");
				return outcome(envelope({ notes_for_next_agent: "persisted seed handoff" }));
			},
			onPhase: progress => {
				if (progress.phase === "seed" && progress.outcome === "advanced") {
					throw new Error("interrupted after integrating seed");
				}
			},
		}).catch((error: unknown) => error);
		expect(failed).toBeInstanceOf(AdwRunError);
		const interrupted = failed as AdwRunError;
		roots.push(path.dirname(interrupted.traceDir));
		expect(firstSeen).toEqual(["dag/seed"]);
		expect(await Bun.file(path.join(cwd, "seed.txt")).text()).toBe("original\n");

		const firstPair = Promise.withResolvers<void>();
		const seen: string[] = [];
		let active = 0;
		let peak = 0;
		let started = 0;
		const resumed = await runAdw({
			host: { cwd, sessionFile: path.join(cwd, "new-session.jsonl") },
			workflow,
			request: "",
			resumeAdwId: interrupted.adwId,
			seatRunner: async seat => {
				seen.push(seat.assignment);
				expect(await Bun.file(path.join(seat.root!, "seed.txt")).text()).toBe("accepted seed\n");
				const name = seat.assignment.split("/")[1]!;
				if (name === "join") {
					expect(active).toBe(0);
					for (const worker of ["a", "b", "c"]) {
						expect(await Bun.file(path.join(seat.root!, `${worker}.txt`)).text()).toBe(`${worker} done\n`);
					}
					return outcome(envelope());
				}
				expect(["a", "b", "c"]).toContain(name);
				expect(seat.task).toContain("persisted seed handoff");
				active++;
				peak = Math.max(peak, active);
				if (++started === 2) firstPair.resolve();
				await firstPair.promise;
				await Bun.write(path.join(seat.root!, `${name}.txt`), `${name} done\n`);
				active--;
				return outcome(envelope());
			},
		});

		expect(resumed.summary.accepted).toBe(true);
		expect(peak).toBe(2);
		expect(seen.filter(name => name !== "dag/join").sort()).toEqual(["dag/a", "dag/b", "dag/c"]);
		expect(resumed.summary.phases.find(phase => phase.name === "seed")?.attempts).toBe(1);
		expect(await Bun.file(path.join(cwd, "seed.txt")).text()).toBe("accepted seed\n");
	}, 30_000);
});
