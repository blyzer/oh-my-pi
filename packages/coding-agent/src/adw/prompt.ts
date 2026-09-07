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
import type { AdwPhaseConfig } from "./types";

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

export function buildPhasePrompt(args: {
	request: string;
	phase: AdwPhaseConfig;
	attempt: number;
	correction?: string;
	handoff?: TaskHandoff;
}): string {
	const { request, phase, attempt, correction, handoff } = args;
	const sections = [`# Request\n${request}`];

	const heading = phase.description ? `\`${phase.name}\` — ${phase.description}` : `\`${phase.name}\``;
	sections.push(`# Your phase\n${heading}`);

	if (handoff) sections.push(`# Handoff from the previous phase\n${renderHandoff(handoff)}`);
	if (phase.prompt) sections.push(`# Phase instructions\n${phase.prompt}`);

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
 * A panel seat answers the same question as everyone else, independently and
 * without writing. No envelope contract here: an opinion is prose, and asking
 * for JSON as well only invites a seat to pretend it did the work.
 */
export function buildPanelPrompt(args: { request: string; phase: AdwPhaseConfig; handoff?: TaskHandoff }): string {
	const { request, phase, handoff } = args;
	const heading = phase.description ? `\`${phase.name}\` — ${phase.description}` : `\`${phase.name}\``;
	const sections = [`# Request\n${request}`, `# Your phase\n${heading}`];
	if (handoff) sections.push(`# Handoff from the previous phase\n${renderHandoff(handoff)}`);
	if (phase.prompt) sections.push(`# Phase instructions\n${phase.prompt}`);
	sections.push(
		`# You are one opinion of ${(phase.panel?.length ?? 0).toString()}\n` +
			"Answer independently. You are READ-ONLY: investigate and decide, but change nothing on disk — " +
			"another agent merges the opinions and does the writing.\n\n" +
			"Be concrete and falsifiable: name files, symbols and commands. State the tradeoff you accepted and " +
			"what would change your mind. Do not hedge across every option — pick one and defend it.",
	);
	return sections.join("\n\n");
}

/**
 * The fuser sees every opinion, labelled by who gave it, and owns the result.
 * Divergence is the payload: agreement is cheap, and the place two strong
 * models disagree is exactly where the decision actually lives.
 */
export function buildFusionPrompt(args: {
	request: string;
	phase: AdwPhaseConfig;
	attempt: number;
	opinions: PanelOpinion[];
	correction?: string;
	handoff?: TaskHandoff;
}): string {
	const { request, phase, attempt, opinions, correction, handoff } = args;
	const heading = phase.description ? `\`${phase.name}\` — ${phase.description}` : `\`${phase.name}\``;
	const sections = [`# Request\n${request}`, `# Your phase\n${heading}`];
	if (handoff) sections.push(`# Handoff from the previous phase\n${renderHandoff(handoff)}`);
	if (phase.prompt) sections.push(`# Phase instructions\n${phase.prompt}`);

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

	if (correction) sections.push(`# Correction (attempt ${attempt})\n${correction}`);
	return sections.join("\n\n");
}
