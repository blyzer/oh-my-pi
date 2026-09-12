/**
 * Pick the workflow that fits a request written in prose.
 *
 * `/adw <name> <request>` needs the name, so the operator must already know
 * which workflows exist and which one suits the work. That is the missing
 * step between "here is what I want" and a run: everything downstream --
 * dispatch, gates, isolation, trace -- assumes the choice was already made.
 *
 * The choice is a selection, never a synthesis. A generated workflow would
 * let the model choose the gates that judge its own output, which is the one
 * thing the acceptance model cannot allow. Picking from what is on disk keeps
 * authorship with the operator and leaves the model a bounded question.
 */

import { runSubprocess } from "../task/executor";
import type { AgentDefinition } from "../task/types";
import type { AdwHost } from "./runner";
import type { DiscoveredWorkflow } from "./types";

/**
 * The classifier could not be asked.
 *
 * Distinct from "no workflow fits": that is a decision the model reached,
 * this is the absence of one. Collapsing them would send an operator off to
 * write a workflow because a provider was rate-limited.
 */
export class ClassifierUnavailableError extends Error {
	constructor(detail: string) {
		super(`could not route this request: ${detail}`);
		this.name = "ClassifierUnavailableError";
	}
}

/** What the classifier decided, and why. */
export interface WorkflowChoice {
	/** The chosen workflow, or null when none of them fits. */
	workflow: DiscoveredWorkflow | null;
	/** One line an operator can judge: why this one, or why none. */
	reason: string;
}

/** Everything the classifier needs to ask a model. */
export interface ClassifyContext {
	host: AdwHost;
	signal?: AbortSignal;
	/**
	 * Model to route with. Routing is a small, bounded judgement, so it does
	 * not need the session's main model -- and pinning it keeps a long
	 * workflow's model choice from silently deciding how requests are routed.
	 */
	model?: string | string[];
	/**
	 * Ask the model. Injected by tests so routing behaviour can be exercised
	 * without a provider, matching how {@link SeatRunner} is substituted in
	 * the workflow driver.
	 */
	ask?: (prompt: string, systemPrompt: string) => Promise<{ output: string; exitCode: number; stderr?: string }>;
}

/**
 * Describe the catalogue for the model.
 *
 * Phases and gates are included, not just names and descriptions: "which
 * workflow suits a bug report" is answerable from the shape of the work --
 * whether a reproducer is required, whether a review gates acceptance --
 * and a name alone hides all of it.
 *
 * @param workflows - Workflows discovered on disk
 * @returns One block per workflow, in catalogue order
 */
function describeCatalogue(workflows: readonly DiscoveredWorkflow[]): string {
	return workflows
		.map(found => {
			const { workflow } = found;
			const phases = workflow.phases
				.map(phase => {
					const gates = phase.gates?.length ? ` gates=[${phase.gates.join(", ")}]` : "";
					return `    - ${phase.name} (${phase.kind})${gates}`;
				})
				.join("\n");
			const description = workflow.description ? `\n  purpose: ${workflow.description}` : "";
			return `- ${workflow.name}${description}\n  phases:\n${phases}`;
		})
		.join("\n\n");
}

const SYSTEM_PROMPT = `You route an engineering request to one of the workflows already defined in this repository.

Answer with a single JSON object and nothing else:
  {"workflow": "<name>", "reason": "<one sentence>"}
  {"workflow": null, "reason": "<what kind of workflow the request would need>"}

Rules:
- Choose only from the catalogue. A name outside it is a wrong answer.
- Choose on the SHAPE OF THE WORK, not on vocabulary overlap with the name.
  A request to fix a defect wants whatever workflow reproduces it first; a
  request to ship wants whatever gates and verifies before delivery.
- Answer null when nothing fits. An ill-fitting workflow runs the wrong
  gates and reports success for work nobody asked for, which is worse for
  the operator than being told to write one.`;

/**
 * Choose a workflow for a request.
 *
 * Refusing is a first-class answer. A wrong pick spends real model budget
 * running gates that do not apply and then reports green, which reads as
 * completed work -- strictly worse than telling the operator no workflow
 * covers this.
 *
 * @param request - The operator's request, in prose
 * @param workflows - Workflows discovered on disk
 * @param ctx - Model access and cancellation
 * @returns The chosen workflow with its reason, or null with a reason
 */
export async function classifyWorkflow(
	request: string,
	workflows: readonly DiscoveredWorkflow[],
	ctx: ClassifyContext,
): Promise<WorkflowChoice> {
	if (workflows.length === 0) {
		return { workflow: null, reason: "no workflows are defined in this repository" };
	}
	// One candidate is not a choice. Asking a model to confirm the only
	// option spends a call to learn nothing, and its "no" would be wrong:
	// the operator asked for this repository's only workflow.
	if (workflows.length === 1 && workflows[0]) {
		return { workflow: workflows[0], reason: `"${workflows[0].workflow.name}" is the only workflow defined here` };
	}

	// Read-only, no tools: the question is answerable from the catalogue in
	// the prompt, and a classifier that can edit files is a classifier that
	// can start the work it was only asked to route.
	const agent: AgentDefinition = {
		name: "adw-classifier",
		description: "Routes a request to one of this repository's workflows",
		systemPrompt: SYSTEM_PROMPT,
		tools: [],
		source: "bundled",
	};
	const task = `Catalogue:\n\n${describeCatalogue(workflows)}\n\nRequest:\n${request}`;
	const result = ctx.ask
		? await ctx.ask(task, SYSTEM_PROMPT)
		: await runSubprocess({
				cwd: ctx.host.cwd,
				agent,
				index: 0,
				id: `adw-classify-${Date.now().toString(36)}`,
				task,
				taskDepth: (ctx.host.taskDepth ?? 0) + 1,
				modelOverride: ctx.model,
				parentActiveModelPattern: ctx.host.activeModelPattern,
				restrictToolNames: true,
				enableIrc: false,
				enableLsp: false,
				settings: ctx.host.settings,
				authStorage: ctx.host.authStorage,
				modelRegistry: ctx.host.modelRegistry,
				signal: ctx.signal,
			});

	// A classifier that could not run has not decided anything. Reporting a
	// quota error or a dead provider as "no workflow fits" would tell the
	// operator to go write one, when the catalogue was never consulted --
	// the same collapse `expect: fail` had to undo between "did not run" and
	// "returned a verdict".
	if (result.exitCode !== 0 || !result.output.trim()) {
		const detail = result.stderr?.trim() || `classifier exited ${result.exitCode}`;
		throw new ClassifierUnavailableError(detail);
	}
	return interpretChoice(result.output, workflows);
}

/**
 * Read the model's answer against the catalogue.
 *
 * Exported for tests: every failure mode here -- prose instead of JSON, a
 * hallucinated name, a missing field -- has to resolve to "no choice" rather
 * than an exception or a wrong run, and that is worth pinning directly.
 *
 * @param output - Raw model output
 * @param workflows - Workflows the answer must name one of
 * @returns The matched workflow, or null with a reason
 */
export function interpretChoice(output: string, workflows: readonly DiscoveredWorkflow[]): WorkflowChoice {
	const parsed = extractObject(output);
	if (!parsed) {
		return { workflow: null, reason: "the classifier did not answer with a workflow choice" };
	}
	const reason = typeof parsed.reason === "string" && parsed.reason.trim() ? parsed.reason.trim() : "no reason given";
	const name = parsed.workflow;
	if (name === null || name === undefined) {
		return { workflow: null, reason };
	}
	if (typeof name !== "string") {
		return { workflow: null, reason: "the classifier did not answer with a workflow choice" };
	}
	const match = workflows.find(found => found.workflow.name === name);
	if (!match) {
		// A name outside the catalogue is not a near miss to be repaired: it
		// is the model inventing a workflow, and running the closest-looking
		// one instead would be a guess about what the operator wanted.
		return { workflow: null, reason: `the classifier named "${name}", which is not a workflow in this repository` };
	}
	return { workflow: match, reason };
}

/**
 * Pull the first JSON object out of model output.
 *
 * A model asked for JSON still opens with "Sure --" often enough that
 * demanding a bare object would fail on answers that are otherwise correct.
 *
 * @param text - Raw model output
 * @returns The parsed object, or null when there is none
 */
function extractObject(text: string): { workflow?: unknown; reason?: unknown } | null {
	const start = text.indexOf("{");
	const end = text.lastIndexOf("}");
	if (start === -1 || end <= start) return null;
	try {
		const value: unknown = JSON.parse(text.slice(start, end + 1));
		if (!value || typeof value !== "object" || Array.isArray(value)) return null;
		return value as { workflow?: unknown; reason?: unknown };
	} catch {
		return null;
	}
}
