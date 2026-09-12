import { logger } from "@oh-my-pi/pi-utils";
import { classifyWorkflow, ClassifierUnavailableError, type WorkflowChoice } from "../adw/classify";
import { AdwConfigError, discoverWorkflows, loadWorkflow } from "../adw/config";
import { type AdwHost, AdwRunError, runAdw } from "../adw/runner";
import { formatModelString } from "../config/model-resolver";
import type { Settings } from "../config/settings";
import type { AgentSession } from "../session/agent-session";
import type { SessionManager } from "../session/session-manager";
import type { EventBus } from "../utils/event-bus";
import { commandConsumed, errorMessage } from "./helpers/parse";
import type { SlashCommandResult, SlashCommandSpec } from "./types";

/**
 * Runs in flight, so a workflow can be stopped. One entry per run: a workflow
 * can spend an hour across several models, and `handle` commands cannot reach
 * the TUI's Esc path on their own — {@link cancelActiveAdwRuns} is what the
 * input controller calls.
 */
const active = new Map<string, { controller: AbortController; workflow: string }>();

export function hasActiveAdwRun(): boolean {
	return active.size > 0;
}

/** Aborts every in-flight workflow. Returns false when there was nothing to stop. */
export function cancelActiveAdwRuns(): boolean {
	if (active.size === 0) return false;
	for (const [key, entry] of active) {
		entry.controller.abort(new Error(`${entry.workflow} interrupted`));
		active.delete(key);
	}
	return true;
}

/**
 * Is `name` a workflow defined in this repository?
 *
 * Separates "named a recipe and forgot the request" from "described the work
 * in one word", which need different answers: a usage line and a routing
 * decision respectively.
 *
 * @param cwd - Directory whose workflows to search
 * @param name - First word of the command
 * @returns True when a workflow goes by that name
 */
async function knownWorkflow(cwd: string, name: string): Promise<boolean> {
	const { workflows } = await discoverWorkflows(cwd);
	return workflows.has(name);
}

/** The host surface a slash-command runtime can supply, plus its output sink. */
interface AdwCommandIo {
	host: AdwHost;
	output: (text: string) => Promise<void> | void;
}

async function runWorkflowCommand(args: string, io: AdwCommandIo): Promise<SlashCommandResult> {
	const { host, output } = io;
	const separator = args.indexOf(" ");
	const name = separator === -1 ? args : args.slice(0, separator);
	const request = separator === -1 ? "" : args.slice(separator + 1).trim();

	if (name === "cancel") {
		await output(cancelActiveAdwRuns() ? "Cancelling the running workflow…" : "No workflow is running.");
		return commandConsumed();
	}

	if (!name || name === "list") {
		const { workflows, problems } = await discoverWorkflows(host.cwd);
		if (workflows.size === 0 && problems.length === 0) {
			await output("No workflows found. Define one at .omp/adw/<name>.yml, then run /adw <name> <request>");
			return commandConsumed();
		}
		const rows = [...workflows.values()].map(found => {
			const phases = found.workflow.phases.map(phase => `${phase.name}:${phase.kind}`).join(" → ");
			const description = found.workflow.description ? ` — ${found.workflow.description}` : "";
			return `  ${found.workflow.name} (${found.level})${description}\n    ${phases}`;
		});
		// A broken file is not an absent file; the operator will ask for it by name.
		const broken = problems.length > 0 ? ["\nFailed to load:", ...problems.map(problem => `  ${problem}`)] : [];
		await output([workflows.size > 0 ? "Workflows:" : "No readable workflows.", ...rows, ...broken].join("\n"));
		return commandConsumed();
	}
	// `/adw resume <workflow> <adw-id>`: the request comes back from the run dir,
	// so the operator does not retype it.
	let resumeAdwId: string | undefined;
	let workflowName = name;
	let phaseRequest = request;
	if (name === "resume") {
		const [target, id] = request.split(/\s+/, 2);
		if (!target || !id) {
			await output("Usage: /adw resume <workflow> <adw-id>  (the id is printed with the trace path)");
			return commandConsumed();
		}
		workflowName = target;
		resumeAdwId = id;
		phaseRequest = "";
	} else if (!request && (await knownWorkflow(host.cwd, name))) {
		// A real workflow name with nothing after it is a missing request. One
		// bare word that is NOT a workflow is a description too short to route,
		// which the classifier answers better than a usage line.
		await output(`Usage: /adw ${name} <request>`);
		return commandConsumed();
	}

	// `/adw <prose>` with no workflow of that name: the operator described the
	// work instead of naming a recipe. Routing it is the step between "here is
	// what I want" and a run -- without it they must already know which
	// workflows exist and which one fits.
	//
	// Only when the first word is not a workflow. A real name always wins, so
	// naming one never costs a model call or risks being second-guessed.
	if (!resumeAdwId) {
		const { workflows } = await discoverWorkflows(host.cwd);
		if (!workflows.has(workflowName)) {
			let choice: WorkflowChoice;
			try {
				choice = await classifyWorkflow(args.trim(), [...workflows.values()], { host, signal: undefined });
			} catch (err) {
				// The classifier never ran, so nothing was decided. Say so and
				// name the workflows: the operator can pick one themselves,
				// which is the whole job routing was saving them.
				if (err instanceof ClassifierUnavailableError) {
					await output(`${err.message}\nPick one yourself: ${[...workflows.keys()].join(", ") || "none defined"}`);
					return commandConsumed();
				}
				throw err;
			}
			if (!choice.workflow) {
				await output(
					`No workflow fits this request: ${choice.reason}\n` +
						`Available: ${[...workflows.keys()].join(", ") || "none"}\n` +
						"Name one explicitly with /adw <name> <request>, or define one at .omp/adw/<name>.yml",
				);
				return commandConsumed();
			}
			// The whole line is the request: the operator never named a phase,
			// so nothing in it was a workflow name to strip.
			workflowName = choice.workflow.workflow.name;
			phaseRequest = args.trim();
			await output(`Routing to "${workflowName}" — ${choice.reason}`);
		}
	}

	const found = await loadWorkflow(host.cwd, workflowName);
	const controller = new AbortController();
	const runKey = `${found.workflow.name}-${Date.now().toString(36)}`;
	active.set(runKey, { controller, workflow: found.workflow.name });

	// `output` is genuinely async over ACP/RPC. Chaining keeps progress lines in
	// order and keeps their rejections inside this function's caller instead of
	// surfacing as unhandled rejections.
	let writes: Promise<void> = Promise.resolve();
	const emit = (text: string) => {
		writes = writes.then(async () => {
			await output(text);
		});
	};

	emit(
		resumeAdwId
			? `Resuming "${found.workflow.name}" (${resumeAdwId}) — Esc or /adw cancel to stop`
			: `Running workflow "${found.workflow.name}" (${found.workflow.phases.length} phases) — Esc or /adw cancel to stop`,
	);
	try {
		const result = await runAdw({
			host,
			workflow: found.workflow,
			request: phaseRequest,
			resumeAdwId,
			signal: controller.signal,
			onPhase: progress => {
				if (!progress.outcome) {
					emit(`▶ ${progress.phase} (${progress.owner}) attempt ${progress.attempt}`);
					return;
				}
				const detail = progress.violations?.[0]?.split("\n")[0];
				emit(`  ${progress.outcome} ${progress.phase}${detail ? ` — ${detail}` : ""}`);
			},
		});

		const lines = result.summary.phases.map(phase => {
			const mark = phase.invalidated ? "-" : phase.passed ? "✓" : "✗";
			const why = phase.violations.length > 0 ? ` — ${phase.violations.join("; ")}` : "";
			return `  ${mark} ${phase.name}${phase.invalidated ? " [invalidated]" : ""} (${phase.owner}, ${phase.attempts} attempt${phase.attempts === 1 ? "" : "s"})${why}`;
		});
		const verdict = result.summary.accepted
			? "accepted"
			: `not accepted${result.summary.reason ? `: ${result.summary.reason}` : ""}`;
		// Whether the work reached the real checkout is the first thing the
		// operator needs to know — never leave it implied.
		const iso = result.isolation;
		const isolationLines = !iso
			? []
			: [
					iso.applied
						? `  isolated (${iso.backend}): ${iso.hadChanges ? "changes applied to your checkout" : "no changes"}`
						: `  isolated (${iso.backend}): NOT applied${iso.conflict ? ` — ${iso.conflict}` : ""}${iso.patchPath ? `\n  patch: ${iso.patchPath}` : ""}`,
					...(iso.nestedPatches > 0
						? [
								iso.nestedApplied
									? `  ${iso.nestedPatches} nested-repo patch(es) applied`
									: `  ${iso.nestedPatches} nested-repo patch(es) captured, not applied`,
							]
						: []),
					...iso.nestedWarnings.map(warning => `  ${warning}`),
				];
		emit(
			[
				`Workflow "${found.workflow.name}" ${verdict}`,
				...lines,
				...isolationLines,
				`  trace: ${result.traceDir}`,
			].join("\n"),
		);
		await writes;
		return commandConsumed();
	} finally {
		active.delete(runKey);
		// Drain whatever the failure path queued before the caller moves on.
		await writes.catch(() => {});
	}
}

/** The host fields both dispatchers can supply from their own session handles. */
function buildHost(parts: {
	cwd: string;
	settings: Settings;
	session: AgentSession;
	sessionManager: SessionManager;
	eventBus?: EventBus;
	subagentEventBus?: EventBus;
}): AdwHost {
	const { cwd, settings, session, sessionManager, eventBus, subagentEventBus } = parts;
	return {
		cwd,
		settings,
		authStorage: session.modelRegistry.authStorage,
		modelRegistry: session.modelRegistry,
		sessionFile: sessionManager.getSessionFile(),
		artifactManager: sessionManager.getArtifactManager() ?? undefined,
		additionalDirectories: sessionManager.getAdditionalDirectories(),
		agentId: session.getAgentId(),
		// Auth-aware fallback: a seat pinned to a provider the operator is not
		// authed for otherwise hard-fails instead of falling back.
		activeModelPattern: session.model ? formatModelString(session.model) : undefined,
		eventBus,
		subagentEventBus,
	};
}

async function reportFailure(
	err: unknown,
	output: (text: string) => Promise<void> | void,
): Promise<SlashCommandResult> {
	// A malformed workflow is the user's typo; anything else is ours. A died run
	// still knows where its trace is — that is the whole record of what it did.
	if (err instanceof AdwRunError) {
		logger.warn("adw run failed", { adwId: err.adwId, error: err.cause });
		await output(`Workflow failed: ${err.message}\n  trace: ${err.traceDir}`);
		return commandConsumed();
	}
	if (!(err instanceof AdwConfigError)) logger.warn("adw command failed", { error: err });
	await output(errorMessage(err));
	return commandConsumed();
}

export const BUILTIN_ADW_SLASH_COMMANDS: ReadonlyArray<SlashCommandSpec> = [
	{
		name: "adw",
		icon: "rocket",
		description: "Run an AI developer workflow from .omp/adw/*.yml",
		inlineHint: "<workflow> <request>",
		allowArgs: true,
		subcommands: [
			{ name: "list", description: "List discovered workflows", usage: "/adw list" },
			{ name: "cancel", description: "Stop the running workflow", usage: "/adw cancel" },
			{
				name: "resume",
				description: "Continue a run that died mid-flight",
				usage: "/adw resume <workflow> <adw-id>",
			},
		],
		handle: async (command, runtime) => {
			try {
				return await runWorkflowCommand(command.args, {
					host: buildHost({
						cwd: runtime.cwd,
						settings: runtime.settings,
						session: runtime.session,
						sessionManager: runtime.sessionManager,
					}),
					output: text => runtime.output(text),
				});
			} catch (err) {
				return await reportFailure(err, text => runtime.output(text));
			}
		},
		// The TUI path exists for the event buses: without them no subagent frame
		// reaches the session, so a fusion phase renders nothing at all while N
		// models run for minutes. They are unreachable from `SlashCommandRuntime`.
		handleTui: async (command, runtime) => {
			const { ctx } = runtime;
			const output = (text: string) => {
				ctx.showStatus(text);
			};
			ctx.editor.setText("");
			try {
				return await runWorkflowCommand(command.args, {
					host: buildHost({
						cwd: ctx.sessionManager.getCwd(),
						settings: ctx.settings,
						session: ctx.session,
						sessionManager: ctx.sessionManager,
						eventBus: ctx.eventBus,
						subagentEventBus: ctx.subagentEventBus,
					}),
					output,
				});
			} catch (err) {
				return await reportFailure(err, output);
			}
		},
	},
];
