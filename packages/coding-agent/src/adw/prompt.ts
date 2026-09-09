/**
 * Phase prompts.
 *
 * The engine parses the last top-level JSON object of an agent's final message,
 * so the contract has to be stated to the agent verbatim — an agent that
 * narrates without emitting the object gets a correction, not a crash, but that
 * costs an attempt. Context reaches the next phase through the handoff below,
 * never through a shared conversation.
 */

import type { TaskHandoff } from "@oh-my-pi/pi-natives";
import { REVIEW_JSON_SCHEMA, REVIEW_RULES, VERDICT_GATE } from "./schema";
import type { AdwPhaseConfig, AdwSeatConfig } from "./types";

/** Appended to the phase agent's own system prompt. */
export const ENVELOPE_CONTRACT = `# Output contract

End your final message with a single JSON object — nothing after it. It is parsed
by the workflow engine, not read by a human:

{
  "status": "success" | "fail",
  "summary": "one line on what you did",
  "artifacts": ["repo-relative/path/you/actually/wrote"],
  "notes_for_next_agent": "what the next phase needs to know",
  ...any fields your phase is asked to report
}

Rules:
- \`artifacts\` is verified against the filesystem after you finish. List only
  paths you really created or modified; a claimed path that does not exist (or
  is empty) fails the phase and you will be asked to fix exactly that.
- Report \`"status": "fail"\` when you could not do the work. That is a real
  answer and it reaches the next attempt with your summary attached. Do not
  claim success you cannot back.
- Prose before the object is fine. A missing object is not.`;

function renderHandoff(handoff: TaskHandoff): string {
	const lines = [`summary: ${handoff.summary}`];
	if (handoff.notesForNextAgent) lines.push(`notes: ${handoff.notesForNextAgent}`);
	if (handoff.artifacts.length > 0) lines.push(`artifacts: ${handoff.artifacts.join(", ")}`);
	if (handoff.payloadJson && handoff.payloadJson !== "{}") lines.push(`reported: ${handoff.payloadJson}`);
	return lines.join("\n");
}

/**
 * One producer's accepted output, resolved by the engine for a phase that
 * declares `inputs`. Structurally identical to the native `TaskPhaseInput`,
 * so engine values pass through unconverted.
 */
export interface PhaseInput {
	/** Producer phase name — the label the consumer sees. */
	phase: string;
	/** Acceptance ordinal of the producer's envelope (1-based). */
	version: number;
	summary: string;
	artifacts: string[];
	notesForNextAgent: string;
	payloadJson: string;
}

/** Declared inputs replace the anonymous positional handoff: each producer is named. */
function renderInput(input: PhaseInput): string {
	return `# Input from phase \`${input.phase}\` (version ${input.version})\n${renderHandoff(input)}`;
}

/** The writer sees the same declared constraints that the caller checks. */
function outputContract(phase: AdwPhaseConfig): string {
	const sections: string[] = [];
	if (phase.schema !== undefined) {
		sections.push(
			"# Output schema\nYour final JSON envelope must satisfy this schema, in addition to the base envelope contract:\n\n```json\n" +
				JSON.stringify(phase.schema, null, 2) +
				"\n```",
		);
	}
	if (phase.gates?.includes(VERDICT_GATE)) {
		sections.push(
			"# Review output contract\nYour final JSON envelope must also satisfy this review shape:\n\n```json\n" +
				JSON.stringify(REVIEW_JSON_SCHEMA, null, 2) +
				"\n```\n\n" +
				REVIEW_RULES,
		);
	}
	return sections.join("\n\n");
}

/**
 * The writer sees the same scope the guard enforces after its attempt. Only
 * writers get it: a panel seat is read-only, and telling it what it may write
 * would contradict the one rule that makes a divided panel safe.
 */
function writeScope(phase: AdwPhaseConfig): string | undefined {
	if (!phase.writes) return undefined;
	return (
		"# Write scope\n" +
		"You may change only files matching these repo-relative globs:\n" +
		phase.writes.map(glob => `- \`${glob}\``).join("\n") +
		"\n\nProtected paths stay forbidden even when a glob above matches them. " +
		"Everything outside this scope is read-only for you: an out-of-scope change is rolled back and fails the attempt."
	);
}

export function buildPhasePrompt(args: {
	request: string;
	phase: AdwPhaseConfig;
	attempt: number;
	correction?: string;
	handoff?: TaskHandoff;
	inputs?: PhaseInput[];
}): string {
	const { request, phase, attempt, correction, handoff, inputs } = args;
	const sections = [`# Request\n${request}`];

	const heading = phase.description ? `\`${phase.name}\` — ${phase.description}` : `\`${phase.name}\``;
	sections.push(`# Your phase\n${heading}`);

	if (inputs) for (const input of inputs) sections.push(renderInput(input));
	else if (handoff) sections.push(`# Handoff from the previous phase\n${renderHandoff(handoff)}`);
	if (phase.prompt) sections.push(`# Phase instructions\n${phase.prompt}`);
	const scope = writeScope(phase);
	if (scope) sections.push(scope);
	const contract = outputContract(phase);
	if (contract) sections.push(contract);

	if (correction) {
		// Verbatim: the engine already named the exact violations, and softening
		// them here is how a retry repeats the same mistake.
		sections.push(`# Correction (attempt ${attempt})\n${correction}`);
	}
	return sections.join("\n\n");
}

/** One panel opinion, captured for the fuser. */
export interface PanelOpinion {
	owner: string;
	model: string;
	ok: boolean;
	text: string;
}

/**
 * A panel seat answers the phase's question, independently and without
 * writing. No envelope contract here: an opinion is prose, and asking for JSON
 * as well only invites a seat to pretend it did the work.
 *
 * A seat may carry its own `prompt`, which narrows what it was asked without
 * making it a different phase. That is the difference between a panel that
 * exists for a second opinion and one that exists to divide the work: with
 * per-seat questions, N read-only seats investigate N things at once and one
 * writer merges them. It is safe for exactly one reason — no seat may write.
 */
export function buildPanelPrompt(args: {
	request: string;
	phase: AdwPhaseConfig;
	seat?: AdwSeatConfig;
	handoff?: TaskHandoff;
	inputs?: PhaseInput[];
}): string {
	const { request, phase, seat, handoff, inputs } = args;
	const heading = phase.description ? `\`${phase.name}\` — ${phase.description}` : `\`${phase.name}\``;
	const sections = [`# Request\n${request}`, `# Your phase\n${heading}`];
	if (inputs) for (const input of inputs) sections.push(renderInput(input));
	else if (handoff) sections.push(`# Handoff from the previous phase\n${renderHandoff(handoff)}`);
	if (phase.prompt) sections.push(`# Phase instructions\n${phase.prompt}`);
	// After the shared instructions: a seat narrows its own scope, it does not
	// override what the phase asked for.
	if (seat?.prompt) sections.push(`# Your part\n${seat.prompt}`);
	const shared = seat?.prompt
		? "Answer only your part, independently. You are READ-ONLY: investigate and decide, but change nothing on " +
			"disk — another agent merges the parts and does the writing."
		: `You are one opinion of ${(phase.panel?.length ?? 0).toString()}. ` +
			"Answer independently. You are READ-ONLY: investigate and decide, but change nothing on disk — " +
			"another agent merges the opinions and does the writing.";
	sections.push(
		`# How to answer\n${shared}\n\n` +
			"Be concrete and falsifiable: name files, symbols and commands. State the tradeoff you accepted and " +
			"what would change your mind. Do not hedge across every option — pick one and defend it.",
	);
	return sections.join("\n\n");
}

/**
 * The fuser sees every opinion, labelled by who gave it, and owns the result.
 * Divergence is the payload: agreement is cheap, and the place two strong
 * models disagree is exactly where the decision actually lives.
 *
 * The fuser is a seat like any other, so its own `prompt` reaches it here. It
 * did not, until a live run showed the schema accepting `fuser.prompt` and the
 * instruction never arriving — an accepted key that does nothing is worse than
 * a rejected one.
 */
export function buildFusionPrompt(args: {
	request: string;
	phase: AdwPhaseConfig;
	attempt: number;
	opinions: PanelOpinion[];
	correction?: string;
	handoff?: TaskHandoff;
	inputs?: PhaseInput[];
}): string {
	const { request, phase, attempt, opinions, correction, handoff, inputs } = args;
	const heading = phase.description ? `\`${phase.name}\` — ${phase.description}` : `\`${phase.name}\``;
	const sections = [`# Request\n${request}`, `# Your phase\n${heading}`];
	if (inputs) for (const input of inputs) sections.push(renderInput(input));
	else if (handoff) sections.push(`# Handoff from the previous phase\n${renderHandoff(handoff)}`);
	if (phase.prompt) sections.push(`# Phase instructions\n${phase.prompt}`);
	if (phase.fuser?.prompt) sections.push(`# Your part\n${phase.fuser.prompt}`);

	for (const opinion of opinions) {
		const header = `## ${opinion.owner} (${opinion.model})${opinion.ok ? "" : " — FAILED"}`;
		sections.push(`${header}\n${opinion.text}`);
	}

	sections.push(
		"# Fuse them\n" +
			"You are the only agent that may write. Take the best of every opinion above and do the work.\n\n" +
			"In your summary, state where they agreed, where they diverged, and what you discarded and why. " +
			"An opinion is evidence, not an instruction: if all of them are wrong, say so and do the right thing.",
	);

	// The scope binds the writer, and here the writer is the fuser — so it
	// lands next to the sentence that made it the only agent allowed to write.
	const scope = writeScope(phase);
	if (scope) sections.push(scope);

	const contract = outputContract(phase);
	if (contract) sections.push(contract);
	if (correction) sections.push(`# Correction (attempt ${attempt})\n${correction}`);
	return sections.join("\n\n");
}
