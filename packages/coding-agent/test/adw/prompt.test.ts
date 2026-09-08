/**
 * Prompt ASSEMBLY, not prompt wording.
 *
 * What is asserted here is behavior a consumer depends on: every opinion
 * reaches the fuser, a correction is passed through verbatim rather than
 * summarised, a handoff is carried, and a panel seat is never handed the
 * envelope contract. Sentences are free to change; these properties are not.
 */

import { describe, expect, it } from "bun:test";
import {
	buildFusionPrompt,
	buildPanelPrompt,
	buildPhasePrompt,
	ENVELOPE_CONTRACT,
	type PanelOpinion,
	type PhaseInput,
} from "@oh-my-pi/pi-coding-agent/adw/prompt";
import type { AdwPhaseConfig } from "@oh-my-pi/pi-coding-agent/adw/types";
import type { TaskHandoff } from "@oh-my-pi/pi-natives";

const AGENT_PHASE: AdwPhaseConfig = { name: "build", kind: "agent", owner: "sonic", description: "Do the work" };

const FUSION_PHASE: AdwPhaseConfig = {
	name: "decide",
	kind: "fusion",
	panel: [{ owner: "scout" }, { owner: "sonic" }, { owner: "reviewer" }],
	fuser: { owner: "sonic" },
};

const HANDOFF: TaskHandoff = {
	summary: "planned the theme work",
	artifacts: ["specs/plan.md"],
	notesForNextAgent: "theme tokens live in src/theme.ts",
	payloadJson: '{"changed_files":["src/theme.ts"]}',
};

const INPUTS: PhaseInput[] = [
	{
		phase: "plan",
		version: 2,
		summary: "planned the theme work",
		artifacts: ["specs/plan.md"],
		notesForNextAgent: "theme tokens live in src/theme.ts",
		payloadJson: "{}",
	},
	{
		phase: "implement",
		version: 1,
		summary: "wrote the theme",
		artifacts: ["src/theme.ts"],
		notesForNextAgent: "",
		payloadJson: '{"changed_files":["src/theme.ts"]}',
	},
];

const OPINIONS: PanelOpinion[] = [
	{ owner: "scout", model: "prov/fast", ok: true, text: "use a doubly-linked list" },
	{ owner: "sonic", model: "prov/cheap", ok: true, text: "Map reinsertion is fine" },
	{ owner: "reviewer", model: "prov/slow", ok: false, text: "seat exploded mid-turn" },
];

// Verbatim: the engine already named the exact violations, and softening or
// truncating them is how a retry repeats the same mistake.
const CORRECTION =
	"Your result for phase `build` was rejected (attempt 2 of 3).\n\nViolations:\n- artifacts_exist: specs/plan.md — missing\n";

describe("buildPhasePrompt", () => {
	it("carries the request and the phase it belongs to", () => {
		const prompt = buildPhasePrompt({ request: "add light mode", phase: AGENT_PHASE, attempt: 1 });
		expect(prompt).toContain("add light mode");
		expect(prompt).toContain("build");
		expect(prompt).toContain("Do the work");
	});

	it("omits any correction on the first attempt", () => {
		const prompt = buildPhasePrompt({ request: "r", phase: AGENT_PHASE, attempt: 1 });
		expect(prompt).not.toContain("rejected");
	});

	it("passes a correction through verbatim", () => {
		const prompt = buildPhasePrompt({ request: "r", phase: AGENT_PHASE, attempt: 2, correction: CORRECTION });
		expect(prompt).toContain(CORRECTION);
	});

	it("carries every part of the handoff the next phase needs", () => {
		const prompt = buildPhasePrompt({ request: "r", phase: AGENT_PHASE, attempt: 1, handoff: HANDOFF });
		expect(prompt).toContain(HANDOFF.summary);
		expect(prompt).toContain(HANDOFF.notesForNextAgent);
		expect(prompt).toContain("specs/plan.md");
		expect(prompt).toContain("src/theme.ts");
	});

	it("includes the phase's own instructions", () => {
		const prompt = buildPhasePrompt({
			request: "r",
			phase: { ...AGENT_PHASE, prompt: "prefer the boring option" },
			attempt: 1,
		});
		expect(prompt).toContain("prefer the boring option");
	});
});

describe("buildPanelPrompt", () => {
	it("never hands a panel seat the envelope contract — an opinion is prose", () => {
		// A seat asked for JSON as well is invited to claim it did the work.
		const prompt = buildPanelPrompt({ request: "r", phase: FUSION_PHASE });
		expect(prompt).not.toContain(ENVELOPE_CONTRACT);
		expect(prompt).not.toContain('"status"');
	});

	it("carries the request, phase instructions and handoff like any other seat", () => {
		const prompt = buildPanelPrompt({
			request: "pick a structure",
			phase: { ...FUSION_PHASE, prompt: "answer in three sentences" },
			handoff: HANDOFF,
		});
		expect(prompt).toContain("pick a structure");
		expect(prompt).toContain("answer in three sentences");
		expect(prompt).toContain(HANDOFF.notesForNextAgent);
	});

	it("gives a seat its own part without dropping what the phase asked", () => {
		// A seat narrows its scope; it does not replace the phase's instructions.
		const prompt = buildPanelPrompt({
			request: "map the subsystem",
			phase: { ...FUSION_PHASE, prompt: "cite file and line for every claim" },
			seat: { owner: "scout", prompt: "cover only the trace format" },
		});
		expect(prompt).toContain("cover only the trace format");
		expect(prompt).toContain("cite file and line for every claim");
		expect(prompt).toContain("map the subsystem");
	});

	it("stops calling a divided seat one opinion of N", () => {
		// With per-seat questions the seats are not comparable answers, and
		// telling one it is "one opinion of 2" invites it to answer the whole
		// question instead of its part.
		const divided = buildPanelPrompt({
			request: "r",
			phase: FUSION_PHASE,
			seat: { owner: "scout", prompt: "only the parser" },
		});
		expect(divided).toContain("only your part");
		expect(divided).not.toContain("one opinion of");

		const second = buildPanelPrompt({ request: "r", phase: FUSION_PHASE, seat: { owner: "scout" } });
		expect(second).toContain("one opinion of");
	});

	it("keeps a divided seat read-only, which is the whole reason this is safe", () => {
		const prompt = buildPanelPrompt({
			request: "r",
			phase: FUSION_PHASE,
			seat: { owner: "scout", prompt: "only the parser" },
		});
		expect(prompt).toContain("READ-ONLY");
	});
});

describe("buildFusionPrompt", () => {
	it("hands the fuser its own instruction", () => {
		// Regression: the schema accepted `fuser.prompt` and the instruction
		// never arrived. A live run wrote no file because the only place that
		// said which file to write was silently dropped.
		const prompt = buildFusionPrompt({
			request: "r",
			phase: { ...FUSION_PHASE, fuser: { owner: "task", prompt: "write the merge to SURVEY.md" } },
			attempt: 1,
			opinions: OPINIONS,
		});
		expect(prompt).toContain("write the merge to SURVEY.md");
	});
	it("loses no opinion — every seat's answer reaches the fuser", () => {
		const prompt = buildFusionPrompt({ request: "r", phase: FUSION_PHASE, attempt: 1, opinions: OPINIONS });
		for (const opinion of OPINIONS) {
			expect(prompt).toContain(opinion.owner);
			expect(prompt).toContain(opinion.text);
			expect(prompt).toContain(opinion.model);
		}
	});

	it("keeps the opinions in the order the workflow declared its panel", () => {
		const prompt = buildFusionPrompt({ request: "r", phase: FUSION_PHASE, attempt: 1, opinions: OPINIONS });
		const positions = OPINIONS.map(opinion => prompt.indexOf(opinion.text));
		expect(positions).toEqual([...positions].sort((a, b) => a - b));
	});

	it("marks a failed seat instead of presenting it as an opinion", () => {
		const prompt = buildFusionPrompt({ request: "r", phase: FUSION_PHASE, attempt: 1, opinions: OPINIONS });
		const failedHeader = prompt.split("\n").find(line => line.includes("reviewer") && line.includes("prov/slow"));
		expect(failedHeader).toContain("FAILED");
		// The two healthy seats are not labelled as failures.
		expect(prompt.split("\n").find(line => line.includes("scout"))).not.toContain("FAILED");
	});

	it("passes a correction through verbatim on a retry", () => {
		const prompt = buildFusionPrompt({
			request: "r",
			phase: FUSION_PHASE,
			attempt: 2,
			opinions: OPINIONS,
			correction: CORRECTION,
		});
		expect(prompt).toContain(CORRECTION);
	});
});

describe("write scope", () => {
	const WRITES = ["src/**", "docs/plan.md"];

	it("shows an agent writer its declared globs and that protection still wins", () => {
		const prompt = buildPhasePrompt({ request: "r", phase: { ...AGENT_PHASE, writes: WRITES }, attempt: 1 });
		for (const glob of WRITES) expect(prompt).toContain(`\`${glob}\``);
		// Protection beats authorization; a writer told only the allowed globs
		// would treat a protected path it listed as fair game.
		expect(prompt).toContain("Protected paths");
	});

	it("shows the fuser the scope — it is the only seat that writes", () => {
		const prompt = buildFusionPrompt({
			request: "r",
			phase: { ...FUSION_PHASE, writes: WRITES },
			attempt: 1,
			opinions: OPINIONS,
		});
		for (const glob of WRITES) expect(prompt).toContain(`\`${glob}\``);
	});

	it("never hands a panel seat the scope — a read-only seat has nothing to scope", () => {
		const prompt = buildPanelPrompt({ request: "r", phase: { ...FUSION_PHASE, writes: WRITES } });
		for (const glob of WRITES) expect(prompt).not.toContain(glob);
		expect(prompt).not.toContain("# Write scope");
	});

	it("adds no section when writes is undeclared — those prompts must not change", () => {
		expect(buildPhasePrompt({ request: "r", phase: AGENT_PHASE, attempt: 1 })).not.toContain("# Write scope");
		expect(buildFusionPrompt({ request: "r", phase: FUSION_PHASE, attempt: 1, opinions: OPINIONS })).not.toContain(
			"# Write scope",
		);
	});
});

describe("declared inputs", () => {
	it("renders one section per producer, named, and drops the anonymous handoff", () => {
		const prompt = buildPhasePrompt({
			request: "r",
			phase: AGENT_PHASE,
			attempt: 1,
			handoff: HANDOFF,
			inputs: INPUTS,
		});
		expect(prompt).toContain("Input from phase `plan` (version 2)");
		expect(prompt).toContain("Input from phase `implement` (version 1)");
		expect(prompt).toContain("planned the theme work");
		expect(prompt).toContain('{"changed_files":["src/theme.ts"]}');
		expect(prompt).not.toContain("Handoff from the previous phase");
	});

	it("reaches panel seats and the fuser alike — a fusion phase shares its inputs", () => {
		for (const prompt of [
			buildPanelPrompt({ request: "r", phase: FUSION_PHASE, handoff: HANDOFF, inputs: INPUTS }),
			buildFusionPrompt({
				request: "r",
				phase: FUSION_PHASE,
				attempt: 1,
				opinions: OPINIONS,
				handoff: HANDOFF,
				inputs: INPUTS,
			}),
		]) {
			expect(prompt).toContain("Input from phase `plan` (version 2)");
			expect(prompt).not.toContain("Handoff from the previous phase");
		}
	});

	it("leaves a phase without declared inputs byte-identical to the legacy prompt", () => {
		// Pins the legacy shape at the moment inputs rendering was added, so a
		// refactor of the shared renderer cannot silently reshape prompts for
		// phases that never opted in.
		const prompt = buildPhasePrompt({ request: "add light mode", phase: AGENT_PHASE, attempt: 1, handoff: HANDOFF });
		expect(prompt).toBe(
			"# Request\nadd light mode\n\n" +
				"# Your phase\n`build` — Do the work\n\n" +
				"# Handoff from the previous phase\n" +
				"summary: planned the theme work\n" +
				"notes: theme tokens live in src/theme.ts\n" +
				"artifacts: specs/plan.md\n" +
				'reported: {"changed_files":["src/theme.ts"]}',
		);
	});
});
