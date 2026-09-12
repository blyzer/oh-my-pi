/**
 * The ADW driver: bridges the Rust phase engine to omp's subagent executor.
 *
 * `TaskRun` (crates/pi-tasks, via `@oh-my-pi/pi-natives`) owns sequencing,
 * retries and acceptance. This file only does what the engine cannot: spawn the
 * model for an `agent` phase, fan out and merge a `fusion` phase, run the
 * command for a `code` phase, and hand the result back. An agent never decides
 * it is done, and a rejected attempt is a correction into the next spawn rather
 * than a restart of the workflow.
 *
 * The roster is omp's own: `owner` resolves through `discoverAgents`, so
 * `.omp/agents/*.md` and the bundled agents are the cast, and a seat may pin a
 * model without redefining who the agent is.
 */

import * as fs from "node:fs/promises";
import * as path from "node:path";
import {
	IsoBackendKind,
	type TaskHandoff,
	type TaskGuardReport,
	type TaskPhaseInput,
	type TaskOutcome,
	type TaskPhaseSpec,
	type TaskStep,
	TaskOutcomeKind,
	TaskPhaseKind,
	TaskRun,
	type TaskRunSummary,
	TaskStepKind,
	TaskWriteGuard,
	TaskTraceReader,
	taskTraceLayout,
} from "@oh-my-pi/pi-natives";
import { getAgentDir, isEnoent, logger, ptree, Snowflake } from "@oh-my-pi/pi-utils";
import { executeShell } from "@oh-my-pi/pi-natives";
import { registerArtifactsDir } from "../internal-urls/registry-helpers";
import { plainArgv } from "./types";
import type { LocalProtocolOptions } from "../internal-urls/local-protocol";
import type { ArtifactManager } from "../session/artifacts";
import type { EventBus } from "../utils/event-bus";
import type { ModelRegistry } from "../config/model-registry";
import { resolveAgentModelSelection } from "../config/model-resolver";
import type { Settings } from "../config/settings";
import type { AuthStorage } from "../session/auth-storage";
import { classifyFailure } from "../task/admission";
import { discoverAgents, getAgent } from "../task/discovery";
import { type ExecutorOptions, runSubagentFollowUpTurn, runSubprocess } from "../task/executor";
import type { AgentDefinition, SingleResult } from "../task/types";
import { parseConfiguredThinkingLevel } from "../thinking";
import { type IsolationContext, prepareIsolationContext } from "../task/isolation-runner";
import { writeIsolationOwner } from "../task/isolation-ownership";
import {
	captureDeltaPatch,
	type DeltaPatchResult,
	cleanupIsolation,
	ensureIsolation,
	getRepoRoot,
	type IsolationHandle,
	parseIsolationBackend,
} from "../task/worktree";
import * as vcs from "@oh-my-pi/pi-natives/vcs";
import { rewindTarget } from "./config";
import { compilePhaseChecks, type PhaseCheckResult, reviewAcceptance, VERDICT_GATE } from "./schema";
import {
	buildFusionPrompt,
	buildPanelPrompt,
	buildPhasePrompt,
	ENVELOPE_CONTRACT_BUNDLED,
	envelopeContract,
	type PanelOpinion,
	type ReviewDiff,
} from "./prompt";
import { describePromptOrigin, loadPrompt } from "./prompt-store";
import type { AdwPhaseConfig, AdwPhaseProgress, AdwSeatConfig, AdwWorkflowConfig } from "./types";
import {
	integrateAccepted,
	preserveDelta,
	recoverIntegration,
	writeRunState,
	type IntegrationRecord,
} from "./integration";

const DEFAULT_COMMAND_TIMEOUT_MS = 10 * 60_000;
/** Command output is quoted into the next agent's correction; keep it useful, not unbounded. */
const COMMAND_SUMMARY_LIMIT = 2_000;
/**
 * Panel seats investigate and decide; they never write. The single-writer rule
 * is what makes fanning out safe in one shared checkout — parallel opinions
 * that could all edit would race each other on the same files.
 */
const PANEL_TOOLS = ["read", "grep", "glob", "yield"];
/**
 * Panel fan-out is the one place ADW spawns concurrently. The session-scoped
 * spawn semaphore is not reachable from a slash command, so the panel honors
 * the same ceiling locally rather than ignoring it and collecting 429s.
 */
const DEFAULT_MAX_PANEL_CONCURRENCY = 4;

/**
 * Everything the driver needs from its host. Deliberately not a `ToolSession`:
 * no slash-command handler can reach one (the instance is a closure local in
 * `sdk.ts`), and every field the executor actually requires is available from
 * `SlashCommandRuntime`.
 */
export interface AdwHost {
	cwd: string;
	settings?: Settings;
	authStorage?: AuthStorage;
	modelRegistry?: ModelRegistry;
	sessionFile?: string | null;
	taskDepth?: number;
	activeModelPattern?: string;
	fallbackModelPattern?: string;
	/**
	 * The parent session's artifact manager. Concurrent seats sharing an
	 * artifacts directory MUST share its id space, or they overwrite each
	 * other's spilled tool output.
	 */
	artifactManager?: ArtifactManager;
	/** `/add-dir` roots, so a seat is not told a file outside cwd does not exist. */
	additionalDirectories?: string[];
	/** Shared `local://` root, so what the fuser writes there is visible to the session. */
	localProtocolOptions?: LocalProtocolOptions;
	/** Registers seats as children of the spawning session rather than orphans. */
	agentId?: string;
	eventBus?: EventBus;
	/**
	 * Answers a `human` phase. The run holds until this resolves, so a host
	 * that cannot ask — a headless CI job, a detached run — should refuse
	 * rather than approve: an unanswerable question is not an approval.
	 *
	 * Omitted entirely, a `human` phase fails closed with that as its reason.
	 */
	decideHuman?: (request: {
		phase: string;
		attempt: number;
		question: string;
	}) => Promise<{ approved: boolean; reason: string }>;
	subagentEventBus?: EventBus;
}

export interface AdwRunOptions {
	host: AdwHost;
	workflow: AdwWorkflowConfig;
	/** The engineer's request, rendered into every phase prompt. Ignored when resuming. */
	request: string;
	/**
	 * Continue the run with this id instead of starting a new one: the engine
	 * rebuilds its cursor, attempts and handoff from that run's trace, and the
	 * request is read back from disk rather than re-supplied.
	 */
	resumeAdwId?: string;
	signal?: AbortSignal;
	/**
	 * Overrides how seats execute. Production leaves it unset and gets real
	 * subagent spawns; a caller that supplies one is choosing what a seat means.
	 */
	seatRunner?: SeatRunner;
	onPhase?: (progress: AdwPhaseProgress) => void;
}

/** What happened to a sandboxed run's changes. */
export interface AdwIsolationOutcome {
	backend: string;
	/** True once the diff is in the real checkout. False means it is only in `patchPath`. */
	applied: boolean;
	hadChanges: boolean;
	/** Where the diff was preserved, when it was not applied. */
	patchPath?: string;
	/** Why an accepted run's patch could not land. */
	conflict?: string;
	/** Nested-repo diffs captured from the sandbox. */
	nestedPatches: number;
	/** True when the nested apply pass ran — only ever after the root landed. */
	nestedApplied: boolean;
	/** Nested repos that would not take their patch. Non-fatal, never silent. */
	nestedWarnings: string[];
}

export interface AdwRunResult {
	adwId: string;
	summary: TaskRunSummary;
	/** Directory holding `events.bin` + `strings.bin` for this run. */
	traceDir: string;
	/** Present only for an isolated run. */
	isolation?: AdwIsolationOutcome;
}

/**
 * A run that died before settling. Carries the trace path because the events
 * already on disk are the only account of what the run did before it failed.
 */
export class AdwRunError extends Error {
	constructor(
		message: string,
		readonly adwId: string,
		readonly traceDir: string,
		options?: ErrorOptions,
	) {
		super(message, options);
		this.name = "AdwRunError";
	}
}

/** Numeric `IsoBackendKind` → its declared name, for reporting which sandbox ran. */
const BACKEND_NAMES = new Map<number, string>(
	Object.entries(IsoBackendKind).map(([name, value]) => [value as number, name.toLowerCase()]),
);
function summarizeOutput(stdout: string, stderr: string): string {
	const combined = `${stdout}\n${stderr}`.trim();
	if (combined.length <= COMMAND_SUMMARY_LIMIT) return combined;
	// Tail, not head: the failure is at the end of a build log.
	return `…${combined.slice(-COMMAND_SUMMARY_LIMIT)}`;
}

/**
 * `Promise.all` with a ceiling. Results stay in input order so a panel's
 * opinions reach the fuser in the order the workflow declared them, not the
 * order the models happened to finish.
 */
export async function mapWithLimit<T, R>(
	items: T[],
	limit: number,
	run: (item: T, index: number) => Promise<R>,
): Promise<R[]> {
	const results: R[] = Array.from({ length: items.length });
	let next = 0;
	const worker = async () => {
		for (let index = next++; index < items.length; index = next++) {
			results[index] = await run(items[index] as T, index);
		}
	};
	await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
	return results;
}

/**
 * Land or preserve a sandboxed run's work.
 *
 * Accepted: the diff is applied to the real checkout, but only after git says
 * it applies cleanly — a half-applied patch is worse than none. Rejected: the
 * patch is kept as an artifact and the checkout is never touched, which is the
 * whole reason for running isolated.
 *
 * Root and nested patches are preflighted together and applied without staging
 * or committing. A durable pending record detects an already-applied patch
 * after interruption; ambiguous partial application fails closed.
 */
export async function settleIsolation(
	isolation: { handle: IsolationHandle; context: IsolationContext },
	accepted: boolean,
	runDir: string,
	adwId: string,
): Promise<AdwIsolationOutcome> {
	// Delivery is a recoverable boundary: once an outcome is recorded, a crash
	// and resume must return it instead of applying the patch a second time.
	const markerPath = path.join(runDir, "delivery.json");
	try {
		return (await Bun.file(markerPath).json()) as AdwIsolationOutcome;
	} catch (err) {
		if (!isEnoent(err)) throw err;
	}
	const outcome = await performSettle(isolation, accepted, runDir, adwId);
	await writeRunState(markerPath, outcome);
	return outcome;
}

async function performSettle(
	isolation: { handle: IsolationHandle; context: IsolationContext },
	accepted: boolean,
	runDir: string,
	adwId: string,
): Promise<AdwIsolationOutcome> {
	const { handle, context } = isolation;
	const pendingPath = path.join(runDir, "delivery.pending.json");
	let pending: { delta: DeltaPatchResult; next: number } | undefined;
	try {
		pending = (await Bun.file(pendingPath).json()) as typeof pending;
	} catch (error) {
		if (!isEnoent(error)) throw error;
	}
	const delta = pending?.delta ?? (await captureDeltaPatch(handle.mergedDir, context.baseline));
	const patch = delta.rootPatch.trim() ? delta.rootPatch : "";
	const base: AdwIsolationOutcome = {
		// `handle.backend` is a numeric N-API enum; "isolated (0)" tells nobody
		// which mechanism actually materialised the sandbox.
		backend: BACKEND_NAMES.get(handle.backend) ?? `kind-${handle.backend}`,
		applied: false,
		hadChanges: patch.length > 0,
		nestedPatches: delta.nestedPatches.length,
		nestedApplied: false,
		nestedWarnings: [],
	};

	const text = patch.endsWith("\n") ? patch : `${patch}\n`;
	// A run can change only a nested repo, leaving the root diff empty; the
	// patch artifact is written only when there is a root diff to write.
	const patchPath = base.hadChanges ? path.join(runDir, `${adwId}.patch`) : undefined;
	if (patchPath) await Bun.write(patchPath, text);
	await preserveDelta(path.join(runDir, "delivery"), delta);
	if (!accepted) return { ...base, patchPath };
	const patches = [{ relativePath: ".", patch: delta.rootPatch }, ...delta.nestedPatches].filter(entry =>
		entry.patch.trim(),
	);
	if (!pending) {
		for (const entry of patches) {
			const repo = vcs.requireGit(path.join(context.repoRoot, entry.relativePath));
			if (!(await repo.canApplyPatch(entry.patch, {}))) {
				return { ...base, patchPath, conflict: `the checkout moved at ${entry.relativePath}; patch preserved` };
			}
		}
	}
	for (let index = pending?.next ?? 0; index < patches.length; index++) {
		const entry = patches[index]!;
		const repo = vcs.requireGit(path.join(context.repoRoot, entry.relativePath));
		if (pending && index === pending.next) {
			const forward = await repo.canApplyPatch(entry.patch, {});
			const reverse = await repo.canApplyPatch(entry.patch, { reverse: true });
			if (!forward && reverse) continue;
			if (!forward || reverse) {
				return {
					...base,
					patchPath,
					conflict: `interrupted delivery is ambiguous at ${entry.relativePath}; patch preserved`,
				};
			}
		}
		await writeRunState(pendingPath, { delta, next: index });
		await repo.applyPatch(entry.patch, {});
	}
	return { ...base, applied: true, patchPath, nestedApplied: delta.nestedPatches.length > 0 };
}

/**
 * How a code phase ended, keeping apart the two things `ok: false` used to
 * collapse.
 *
 * `ran` means the command executed and the shell reported its own exit code.
 * `infrastructure` means it never got to render a verdict: a timeout, a
 * cancellation, a signal, a spawn that failed. Both are failures, but only
 * the first is evidence about the WORK — and `expect: fail` is unsound
 * without the distinction, since a reproducer that never ran would otherwise
 * count as "failed as required".
 */
/**
 * Signal names for the `128 + N` convention, used to word an exit status --
 * never to classify one.
 */
const SIGNAL_NAMES: Record<number, string> = {
	1: "SIGHUP",
	2: "SIGINT",
	3: "SIGQUIT",
	6: "SIGABRT",
	9: "SIGKILL",
	11: "SIGSEGV",
	13: "SIGPIPE",
	15: "SIGTERM",
};

/**
 * Word an exit status for a human reader.
 *
 * `sh` reports a child killed by signal N as `128 + N`, and a raw "exited
 * 143" tells the next agent its build failed when the host was shutting
 * down -- it then spends its attempts fixing nothing.
 *
 * This only phrases the summary. The verdict stays exactly what the status
 * says, because `exit 143` is a status a command may legitimately choose
 * and nothing here can tell the two apart.
 *
 * @param exitCode - Exit status the shell reported
 * @returns A human phrase describing the status
 */
function describeExit(exitCode: number): string {
	if (exitCode > 128) {
		const name = SIGNAL_NAMES[exitCode - 128];
		if (name) return `exited ${exitCode} (the status sh reports for a child killed by ${name})`;
	}
	return `exited ${exitCode}`;
}

/**
 * Judge a command against what the workflow expected of it.
 *
 * The default is the ordinary gate: green passes. `fail` inverts it, and
 * exists for the one thing a green suite cannot express — a bug reproducer
 * must be RED before the fix, or nothing proves it exercises the fault. A
 * workflow that only ever demands green accepts a fix whose test never
 * reproduced the bug.
 *
 * Inversion applies ONLY to a command that ran and chose its exit code. A
 * timeout, a cancellation, a signal or a failed spawn never rendered a
 * verdict, so counting them as "failed as required" would accept a
 * reproducer that never executed — the exact hole `expect: fail` is meant to
 * close, reopened from the other side.
 */
export function judgeExpectation(phase: AdwPhaseConfig, result: CodePhaseResult): [accepted: boolean, summary: string] {
	if ((phase.expect ?? "pass") !== "fail") return [result.ok, result.summary];
	if (result.kind === "infrastructure") {
		return [false, `expected ${phase.name} to fail, but it never ran: ${result.summary}`];
	}
	return result.ok
		? [
				false,
				`expected ${phase.name} to fail, but it passed — the reproducer does not exercise the fault: ${result.summary}`,
			]
		: [true, `expected to fail and did (exit ${result.exitCode}): ${result.summary}`];
}

export type CodePhaseResult =
	| { kind: "ran"; ok: boolean; exitCode: number; summary: string }
	| { kind: "infrastructure"; ok: false; summary: string };

export async function runCodePhase(
	phase: AdwPhaseConfig,
	cwd: string,
	signal: AbortSignal | undefined,
	/** Extra variables for this command only, on top of the inherited environment. */
	env?: Record<string, string>,
): Promise<CodePhaseResult> {
	const command = phase.command ?? "";
	const timeout = phase.timeoutMs ?? DEFAULT_COMMAND_TIMEOUT_MS;
	// A phase that inverts its criterion is the one case where "the command
	// did not run" and "the command failed" must not collapse: a typo would
	// otherwise satisfy MUST_FAIL and the workflow would accept a fix no
	// test ever exercised.
	//
	// No status can settle it -- 127 is a convention any command may return,
	// and stderr is the command's own output -- so launch plain argv
	// directly and let the kernel answer. A failed exec raises ENOENT or
	// EACCES before any process exists; anything that starts renders a
	// verdict. One execution either way: probing the filesystem first would
	// still miss a script whose shebang interpreter is gone, and spawning
	// twice would repeat a side effect.
	//
	// The paired green phase takes this path too. A pair only repeats the
	// same test if both halves run it the same way -- the loader marks the
	// counterpart so quoting and word splitting cannot differ between them.
	const directArgv = phase.expect === "fail" || phase.directLaunch ? plainArgv(command) : null;
	if (directArgv) {
		try {
			const result = await ptree.exec(directArgv, {
				cwd,
				timeout,
				allowNonZero: true,
				allowAbort: true,
				signal,
				env: env ? { ...(process.env as Record<string, string>), ...env } : undefined,
			});
			const summary = summarizeOutput(result.stdout, result.stderr);
			if (result.exitError?.aborted) {
				const why = signal?.aborted ? "was cancelled" : `was killed after ${timeout}ms`;
				return { kind: "infrastructure", ok: false, summary: `${command} ${why}\n${summary}` };
			}
			if (result.exitCode === null) {
				return { kind: "infrastructure", ok: false, summary: `${command} ended without an exit code\n${summary}` };
			}
			return {
				kind: "ran",
				ok: result.exitCode === 0,
				exitCode: result.exitCode,
				summary:
					result.exitCode === 0
						? summary || `${command} exited 0`
						: `${command} ${describeExit(result.exitCode)}\n${summary}`,
			};
		} catch (error) {
			if (isEnoent(error)) {
				return { kind: "infrastructure", ok: false, summary: `${command} could not be found to run` };
			}
			if (error && typeof error === "object" && "code" in error && error.code === "EACCES") {
				return { kind: "infrastructure", ok: false, summary: `${command} could not be executed` };
			}
			throw error;
		}
	}
	try {
		// Absolute shell on POSIX (a launcher may hand omp a minimal tool-only
		// PATH); the vendored Brush shell on Windows, where /bin/sh does not
		// exist. Same split as runShellCommand in config/resolve-config-value.ts.
		if (process.platform === "win32") {
			let output = "";
			const result = await executeShell({ command, cwd, timeoutMs: timeout, signal, env }, (err, chunk) => {
				if (!err) output += chunk;
			});
			// Three ways to end without a verdict, and `exitCode` is optional
			// precisely because of them. Defaulting it to a number would
			// manufacture a verdict the shell never rendered, and MUST_FAIL
			// would read a cancelled command as "failed as required".
			if (result.timedOut || result.cancelled || result.exitCode === undefined) {
				const why = result.timedOut
					? `timed out after ${timeout}ms`
					: result.cancelled
						? "was cancelled"
						: "ended without an exit code";
				return { kind: "infrastructure", ok: false, summary: `${command} ${why}\n${output.trim()}` };
			}
			const ok = result.exitCode === 0;
			return {
				kind: "ran",
				ok,
				exitCode: result.exitCode,
				summary: ok
					? output.trim() || `${command} exited 0`
					: `${command} exited ${result.exitCode}\n${output.trim()}`,
			};
		}
		const result = await ptree.exec(["/bin/sh", "-c", command], {
			cwd,
			timeout,
			allowNonZero: true,
			allowAbort: true,
			signal,
			// Bun replaces the environment wholesale; merge so PATH survives.
			env: env ? { ...(process.env as Record<string, string>), ...env } : undefined,
		});
		const summary = summarizeOutput(result.stdout, result.stderr);
		// `allowAbort` means a timeout or cancellation returns normally, and the
		// command can still exit 0 in the race between the deadline firing and
		// the kill landing. Reading exitCode alone writes a killed test suite
		// into the trace as a passing gate.
		if (result.exitError?.aborted) {
			// Only one of these is a deadline. Telling an agent its build "timed
			// out" when the operator pressed Esc sends it hunting a slow test.
			const why = signal?.aborted ? "was cancelled" : `was killed after ${timeout}ms`;
			return { kind: "infrastructure", ok: false, summary: `${command} ${why}\n${summary}` };
		}
		// Everything below this point RAN. A shell exit status cannot say
		// otherwise: `exit 143` looks like SIGTERM, `exit 127` like a missing
		// command, and a test that prints "command not found" and exits 127
		// looks like both -- yet all three are verdicts the command chose.
		//
		// `ptree` reports the two endings it can actually attest to, and they
		// are handled above: `exitError.aborted` for a timeout or a
		// cancellation, and a missing exit code for a process that never
		// produced one. It exposes no signal or spawn-failure field, so any
		// further classification here would be a guess about the command's
		// own output -- and guessing wrong reclassifies a legitimate red run
		// as a broken environment, which is exactly what `expect: fail`
		// depends on not happening.
		if (result.exitCode === null) {
			// Same reasoning as the Windows branch: no exit code means no
			// verdict, whatever else the runner reported.
			return { kind: "infrastructure", ok: false, summary: `${command} ended without an exit code\n${summary}` };
		}
		return {
			kind: "ran",
			ok: result.exitCode === 0,
			exitCode: result.exitCode,
			summary:
				result.exitCode === 0
					? summary || `${command} exited 0`
					: `${command} ${describeExit(result.exitCode)}\n${summary}`,
		};
	} catch (err) {
		return {
			kind: "infrastructure",
			ok: false,
			summary: `${command} could not run: ${err instanceof Error ? err.message : String(err)}`,
		};
	}
}

/**
 * A writer seat: the agent's own prompt plus the envelope contract it must
 * satisfy. The contract is resolved against `cwd` so an operator override
 * reaches the seat that has to honour it.
 */
function deriveWriterAgent(base: AgentDefinition, cwd: string): AgentDefinition {
	return { ...base, systemPrompt: `${base.systemPrompt}\n\n${envelopeContract(cwd)}` };
}

/** A panel seat: same agent, read-only, and no envelope contract — it owes an opinion, not a result. */
function derivePanelAgent(base: AgentDefinition): AgentDefinition {
	return { ...base, tools: PANEL_TOOLS, spawns: undefined, prewalk: undefined };
}

/** One seat's assignment, independent of how it is executed. */
export interface SeatRequest {
	seat: string;
	agent: AgentDefinition;
	task: string;
	id: string;
	index: number;
	description: string;
	assignment: string;
	/** Panel seats are read-only; writer seats are not. */
	readOnly: boolean;
	/** Model pattern pinned by the workflow, if any. */
	model?: string;
	thinking?: string;
	/**
	 * Continue an existing seat with a correction or a new assignment after its
	 * previous result was invalidated. A fresh spawn still receives `task`.
	 */
	followUpMessage?: string;
	/** Working directory override: a concurrent writer runs in its own workspace. */
	root?: string;
	/** Cancellation for this dispatch alone, e.g. invalidation by a revision. */
	signal?: AbortSignal;
}

/** What one writer attempt produced, before integration and judgment. */
type WriterExecution =
	| { kind: "seat"; role: "agent" | "fuser"; seatName: string; result: SeatOutcome }
	| { kind: "panel-failed"; message: string };

/** What a seat produced. Narrower than `SingleResult` — only what the driver reads. */
export interface SeatOutcome {
	output: string;
	stderr: string;
	exitCode: number;
	tokens: number;
	/** The model that actually answered, after fallbacks. */
	model: string;
}

/**
 * Executes one seat. Injectable so the driver's decisions — which seat runs,
 * whether a retry continues or respawns, whether a retried fusion phase
 * re-polls its panel — are testable without a model, while production supplies
 * the real subagent executor.
 */
export type SeatRunner = (request: SeatRequest) => Promise<SeatOutcome>;

/**
 * The executor options for one seat.
 *
 * Pure and exported because two of these fields are safety invariants rather
 * than plumbing: `enableIrc: false` is what keeps a panel seat from reading its
 * siblings' live activity, and `parentArtifactManager` is what keeps concurrent
 * seats from overwriting each other's spilled output. A regression in either is
 * silent at runtime, so the contract is asserted directly.
 */
export function buildSeatSpawnOptions(args: {
	seat: SeatRequest;
	host: AdwHost;
	workRoot: string;
	artifactsDir: string;
	modelPatterns: string[];
	modelRole: string | undefined;
	signal?: AbortSignal;
}): ExecutorOptions {
	const { seat, host, workRoot, artifactsDir, modelPatterns, modelRole, signal } = args;
	return {
		cwd: workRoot,
		agent: seat.agent,
		task: seat.task,
		assignment: seat.assignment,
		description: seat.description,
		index: seat.index,
		id: seat.id,
		taskDepth: host.taskDepth ?? 0,
		modelOverride: modelPatterns,
		modelRole,
		parentActiveModelPattern: host.activeModelPattern,
		thinkingLevel: parseConfiguredThinkingLevel(seat.thinking) ?? seat.agent.thinkingLevel,
		restrictToolNames: seat.readOnly,
		// `restrictToolNames` drops the `hub` TOOL but leaves the executor's IRC
		// prompt on, which renders a live peer roster naming the sibling seats and
		// what each is currently doing. A panel seat that can read that is not an
		// independent opinion.
		enableIrc: !seat.readOnly,
		// `task.enableLsp` defaults to false; the executor's own default is true,
		// so omitting this silently overrides the operator and boots an LSP server
		// set per seat.
		enableLsp: !seat.readOnly && (host.settings?.get("task.enableLsp") ?? false),
		settings: host.settings,
		authStorage: host.authStorage,
		modelRegistry: host.modelRegistry,
		sessionFile: host.sessionFile,
		persistArtifacts: Boolean(host.sessionFile),
		artifactsDir,
		// Shared id space: without the parent's manager every concurrent seat
		// seeds its own counter from the same directory scan and they overwrite
		// each other's spilled artifacts.
		parentArtifactManager: host.artifactManager,
		additionalDirectories: host.additionalDirectories,
		localProtocolOptions: host.localProtocolOptions,
		parentAgentId: host.agentId,
		eventBus: host.eventBus,
		subagentEventBus: host.subagentEventBus,
		// Lifecycle frames reach the host's surfaces through the buses above; a
		// second per-seat callback would duplicate them.
		signal,
	};
}

/**
 * The production {@link SeatRunner}: real subagent spawns.
 *
 * On a retry (`followUpMessage`) it continues the seat's existing session
 * instead of respawning — the seat already remembers the request and its own
 * rejected answer, so the correction costs one message rather than a cold
 * restart, and a violation like "the file you claimed is missing" is only
 * actionable to an agent that has seen what it claimed. Falls back to a fresh
 * spawn when that session is gone.
 */
export function createExecutorSeatRunner(ctx: {
	host: AdwHost;
	workRoot: string;
	artifactsDir: string;
	signal?: AbortSignal;
}): SeatRunner {
	const { host, workRoot, artifactsDir, signal } = ctx;
	const agentModelOverrides = host.settings?.get("task.agentModelOverrides") ?? {};
	const seatRoots = new Map<string, string>();

	return async (seat: SeatRequest): Promise<SeatOutcome> => {
		const { patterns, role } = resolveAgentModelSelection({
			requestModel: seat.model,
			settingsOverride: agentModelOverrides[seat.agent.name],
			agentModel: seat.agent.model,
			settings: host.settings,
			activeModelPattern: host.activeModelPattern,
			fallbackModelPattern: host.fallbackModelPattern,
		});
		// The model that actually answered, not the one requested: two seats
		// pinned to unavailable providers can both land on the same fallback, and
		// labelling them differently would tell the fuser it has two opinions
		// when it has one.
		const toOutcome = (result: SingleResult): SeatOutcome => ({
			output: result.output,
			stderr: result.stderr,
			exitCode: result.exitCode,
			// `result.tokens` excludes cacheRead by design — it is a cumulative
			// billing-volume counter, and re-reading cached context every turn
			// would make that sum misleading. For "what did this attempt cost"
			// that exclusion is wrong: a cache-warm turn reports zero, which
			// measured as literally 0 for a real run that wrote a file. Prefer
			// the aggregated total and fall back only when no usage arrived.
			tokens: result.usage?.totalTokens ?? result.tokens,
			model: result.resolvedModel ?? patterns[0] ?? seat.agent.model?.[0] ?? "default",
		});

		const root = seat.root ?? workRoot;
		const canContinue = seatRoots.get(seat.id) === root;
		seatRoots.set(seat.id, root);
		if (seat.followUpMessage && canContinue) {
			try {
				return toOutcome(
					await runSubagentFollowUpTurn({
						id: seat.id,
						agent: seat.agent,
						message: seat.followUpMessage,
						index: seat.index,
						description: seat.description,
						modelRole: role,
						signal: seat.signal ?? signal,
						eventBus: host.eventBus,
						subagentEventBus: host.subagentEventBus,
						artifactsDir,
					}),
				);
			} catch (err) {
				// Parked past its TTL, disposed, or never registered: a fresh spawn
				// still carries the correction in its prompt, just without memory.
				logger.debug("adw could not continue seat; respawning", { id: seat.id, error: err });
			}
		}
		return toOutcome(
			await runSubprocess(
				buildSeatSpawnOptions({
					seat,
					host,
					workRoot: seat.root ?? workRoot,
					artifactsDir,
					modelPatterns: patterns,
					modelRole: role,
					signal: seat.signal ?? signal,
				}),
			),
		);
	};
}
/** Bound the diff a reviewer carries: enough to judge, not enough to blow a context window. */
const REVIEW_DIFF_MAX_CHARS = 60_000;

/**
 * The change-set a review phase is judging, read from the working tree.
 *
 * A reviewer that sees only the writer's envelope can check that the account
 * is coherent, never that it is true — the two are different questions, and
 * only the second is a review. `diff_matches_claims` already proves every
 * changed path was declared; this supplies what was written to them.
 *
 * Returns undefined when the tree is not a repository or git fails: a review
 * without the diff is weaker, but refusing to run it would turn a missing
 * tool into a failed acceptance.
 *
 * @param root - Working tree the phase ran against
 * @param signal - Cancellation from the owning phase
 * @returns The change-set, or undefined when it cannot be read
 */
export async function collectReviewDiff(root: string, signal?: AbortSignal): Promise<ReviewDiff | undefined> {
	try {
		const names = await ptree.exec(["git", "status", "--porcelain=v1", "-z"], {
			cwd: root,
			allowNonZero: true,
			signal,
			timeout: 15_000,
		});
		if (names.exitCode !== 0) return undefined;
		const paths = names.stdout
			.split("\0")
			.filter(Boolean)
			// Porcelain v1 prefixes two status columns and a space.
			.map(entry => entry.slice(3))
			.filter(Boolean)
			.sort();
		if (paths.length === 0) return { paths: [], patch: "", truncated: false };

		// `HEAD` covers staged and unstaged alike; untracked files have no blob
		// to diff, so they are named above and their content is not shown.
		const patch = await ptree.exec(["git", "diff", "HEAD", "--"], {
			cwd: root,
			allowNonZero: true,
			signal,
			timeout: 30_000,
		});
		if (patch.exitCode !== 0) return { paths, patch: "", truncated: true };
		const full = patch.stdout;
		const truncated = full.length > REVIEW_DIFF_MAX_CHARS;
		return { paths, patch: truncated ? full.slice(0, REVIEW_DIFF_MAX_CHARS) : full, truncated };
	} catch {
		return undefined;
	}
}

/**
 * Whether the trace's CURRENT terminal record says the run was accepted.
 * The last `run_finished` wins: a crash writes a failed terminal too, and a
 * continuation's verdict supersedes it. Unreadable or terminal-less traces
 * answer `false` and leave the explaining to the engine's own resume checks.
 */
function traceAccepted(traceDir: string): boolean {
	try {
		const reader = new TaskTraceReader(traceDir);
		const raw = reader.readRaw(0);
		const layout = taskTraceLayout();
		let accepted: boolean | null = null;
		for (let at = 0; at + layout.recordLen <= raw.length; at += layout.recordLen) {
			if (layout.kindNames[raw[at + layout.offKind] as number] !== "run_finished") continue;
			accepted = ((raw[at + layout.offFlags] as number) & layout.flagOk) !== 0;
		}
		return accepted === true;
	} catch {
		return false;
	}
}

export async function runAdw(options: AdwRunOptions): Promise<AdwRunResult> {
	const { host, workflow, signal, onPhase } = options;
	const concurrency = workflow.concurrency ?? 1;
	if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 8) {
		throw new Error("workflow concurrency must be an integer from 1 through 8");
	}
	if (concurrency > 1 && (!workflow.isolation || workflow.phases.some(p => p.kind !== "code" && !p.writes))) {
		throw new Error("concurrent workflows require isolation and explicit writes on every writer");
	}
	const resuming = options.resumeAdwId;
	const adwId = resuming ?? `adw-${Snowflake.next()}`;

	// Run state (trace, request, envelopes) lives OUTSIDE the session directory.
	// A crashed run is resumed by a NEW process, and a new process means a new
	// session dir — keying run state by session would make the dead run
	// unfindable by exactly the caller resume exists for. Seat artifacts still
	// go to the session dir, so `artifact://` keeps resolving.
	const runDir = path.join(getAgentDir(), "adw", adwId);
	await fs.mkdir(runDir, { recursive: true });
	const sessionArtifactsDir = host.sessionFile ? host.sessionFile.slice(0, -6) : null;
	if (!sessionArtifactsDir) registerArtifactsDir(runDir);
	const traceDir = path.join(runDir, "trace");

	// The request is not in the trace, so a resume would have to re-type it.
	// Persisting it makes `/adw resume <id>` sufficient on its own.
	const requestPath = path.join(runDir, "request.txt");
	const recorded = resuming
		? await Bun.file(requestPath)
				.text()
				.catch(() => "")
		: options.request;
	if (!recorded.trim()) {
		throw new Error(
			resuming
				? `No resumable run "${resuming}": ${requestPath} does not exist. /adw list shows workflows; the run id is printed with the trace path.`
				: "A workflow needs a request",
		);
	}
	const request = recorded;
	if (!resuming) await Bun.write(requestPath, request);

	const deliveryPath = path.join(runDir, "delivery.json");
	const workflowRecordPath = path.join(runDir, "workflow.json");
	const isolationRecordPath = path.join(runDir, "isolation.json");
	if (resuming) {
		// The contract is what the trace's decisions were made against; replaying
		// them under an edited workflow would silently substitute a different
		// plan. Legacy runs without the record are tolerated as before.
		const persisted: unknown = await Bun.file(workflowRecordPath)
			.json()
			.catch((error: unknown) => {
				if (isEnoent(error) && concurrency === 1) return undefined;
				throw error;
			});
		if (persisted && !Bun.deepEquals(JSON.parse(JSON.stringify(workflow)) as unknown, persisted)) {
			throw new Error(
				`The workflow definition changed since run "${adwId}" started; resume replays its decisions against the original contract. ` +
					`Restore the definition recorded at ${workflowRecordPath}, or start a new run.`,
			);
		}
		if (await Bun.file(deliveryPath).exists()) {
			throw new Error(
				`Run "${adwId}" already settled its delivery — resuming would risk applying it twice. The recorded outcome is at ${deliveryPath}.`,
			);
		}
	} else {
		await writeRunState(workflowRecordPath, workflow);
	}

	// Resolve the whole cast before spending a token: an unknown owner must fail
	// the run at step zero, not halfway through a build.
	const discovery = await discoverAgents(host.cwd);
	const roster = new Map<string, AgentDefinition>();
	const requireAgent = (owner: string, phaseName: string) => {
		if (roster.has(owner)) return;
		const agent = getAgent(discovery.agents, owner);
		if (!agent) {
			const available = discovery.agents.map(candidate => candidate.name).join(", ") || "none";
			throw new Error(
				`Workflow "${workflow.name}" phase "${phaseName}" names unknown agent "${owner}". Available: ${available}`,
			);
		}
		roster.set(owner, agent);
	};
	for (const phase of workflow.phases) {
		if (phase.kind === "agent" && phase.owner) requireAgent(phase.owner, phase.name);
		if (phase.kind === "fusion") {
			for (const seat of phase.panel ?? []) requireAgent(seat.owner, phase.name);
			if (phase.fuser) requireAgent(phase.fuser.owner, phase.name);
		}
	}

	// Compile before allocating a sandbox: invalid contracts must never leak one.
	const phaseChecks = new Map<string, (turn: string) => PhaseCheckResult>();
	for (const phase of workflow.phases) {
		if (phase.kind === "code" || phase.kind === "human") continue;
		const compiled = compilePhaseChecks(phase);
		if (typeof compiled === "string") throw new Error(`Phase "${phase.name}" has an unusable contract: ${compiled}`);
		phaseChecks.set(phase.name, compiled.check);
	}

	// One sandbox for the WHOLE workflow, not one per spawn: `plan` writes files
	// that `build` reads and that gates verify, so per-spawn isolation would
	// throw away the continuity the phases depend on. Changes reach the real
	// checkout once, at the end, and only if the run is accepted.
	let isolation: { handle: IsolationHandle; context: IsolationContext } | undefined;
	let traceRun: TaskRun | undefined;
	let completed = false;
	let guard: TaskWriteGuard | undefined;
	/** The guarded phase between begin() and settle(), for crash-path recovery. */
	let guardBoundary: AdwPhaseConfig | null = null;
	const settleGuard = (phase: AdwPhaseConfig): TaskGuardReport | undefined => {
		const report = guard?.settle({
			allowed: phase.writes,
			protectedGlobs: workflow.protected ?? [],
			patchDir: runDir,
		});
		guardBoundary = null;
		return report;
	};
	/** Fail closed: a tree the guard could not restore must never settle as success. */
	const requireRecovered = (report: TaskGuardReport | undefined) => {
		if (!report?.unrecoverable.length) return;
		throw new Error(
			`write guard could not restore ${report.unrecoverable.join(", ")}; ` +
				`the unauthorized diff is preserved at ${report.patchPath ?? runDir}`,
		);
	};
	// Include setup in cleanup: even a bad engine config must release its sandbox.
	try {
		if (workflow.isolation) {
			if (resuming) {
				// Reattach the surviving sandbox; NEVER rebuild it from the base —
				// the completed phases' work exists only inside it, and a fresh
				// materialisation would silently redo them from the base commit.
				const record = (await Bun.file(isolationRecordPath)
					.json()
					.catch((error: unknown) => {
						// Missing record = missing sandbox: same refusal below. A
						// corrupt record must never silently rebuild from base.
						if (isEnoent(error)) return null;
						throw new Error(`cannot read isolated run state: ${error}`);
					})) as { backend: number; mergedDir: string; context: IsolationContext } | null;
				const alive = record
					? await fs.access(record.mergedDir).then(
							() => true,
							() => false,
						)
					: false;
				if (!record || !alive) {
					throw new Error(
						`Workflow "${workflow.name}" runs isolated and the sandbox for "${adwId}" is gone — ` +
							`it cannot be rebuilt from the base without silently redoing completed phases. ` +
							`If the run settled, its diff was preserved as a patch; otherwise re-run.`,
					);
				}
				// Re-claim ownership so a sweeping `omp worktree clear` does not
				// reap the sandbox out from under the resumed run.
				await writeIsolationOwner(path.dirname(record.mergedDir), adwId);
				isolation = {
					handle: { mergedDir: record.mergedDir, backend: record.backend, fellBack: false, fallbackReason: null },
					context: record.context,
				};
			} else {
				const context = await prepareIsolationContext(host.cwd);
				const backend = parseIsolationBackend(host.settings?.get("isolation.backend") ?? "auto");
				const handle = await ensureIsolation(context.repoRoot, adwId, backend);
				isolation = { handle, context };
				await writeRunState(isolationRecordPath, {
					backend: handle.backend,
					mergedDir: handle.mergedDir,
					context,
				});
			}
			logger.debug("adw isolated", {
				adwId,
				backend: isolation.handle.backend,
				dir: isolation.handle.mergedDir,
				resumed: Boolean(resuming),
			});
		}
		/** Where phases actually run. The roster is still discovered from the real cwd. */
		const workRoot = isolation?.handle.mergedDir ?? host.cwd;
		if (resuming && concurrency > 1) await recoverIntegration(workRoot, runDir, traceDir);

		if (resuming && isolation && traceAccepted(traceDir)) {
			// The run accepted and then died inside the delivery window. The
			// decision is already on the trace; only the settlement is owed, and
			// the marker inside settleIsolation makes it happen exactly once.
			const applied = await settleIsolation(isolation, true, runDir, adwId);
			logger.debug("adw recovered delivery", { adwId, applied: applied.applied });
			return {
				adwId,
				summary: {
					adwId,
					workflow: workflow.name,
					accepted: true,
					reason: "recovered the delivery of an accepted run interrupted during settlement",
					phases: [],
				},
				traceDir,
				isolation: applied,
			};
		}

		const phasesByName = new Map(workflow.phases.map(phase => [phase.name, phase]));
		/**
		 * The code phase that judges a writer's delivery: a `code` phase
		 * depending on it whose OTHER dependencies have already landed.
		 *
		 * The qualifier is the whole difficulty. A join gate like
		 * `left && right` is not a verdict on `left` alone — running it when
		 * only `left` has landed fails for a reason that is not a fault, and
		 * rejects work that was correct. Only the landing that completes the
		 * gate's inputs can be judged by it.
		 *
		 * No such phase means the workflow declared no deterministic check
		 * this landing completes, and there is nothing to re-run.
		 */
		const deliveredCheckFor = (
			writer: AdwPhaseConfig,
			landed: ReadonlySet<string>,
		): ((deliveredRoot: string) => Promise<{ ok: true } | { ok: false; evidence: string }>) | undefined => {
			const judge = workflow.phases.find(candidate => {
				if (candidate.kind !== "code") return false;
				// A gate reading `ADW_INPUTS` is judging the envelopes its
				// dispatch selected, not the tree. Those versions do not exist
				// at landing time, so re-running it here would fail for want
				// of context rather than for a fault in the delivery.
				if (candidate.inputs?.length) return false;
				const deps = candidate.dependsOn ?? [];
				if (!deps.includes(writer.name)) return false;
				return deps.every(dep => dep === writer.name || landed.has(dep));
			});
			if (!judge) return undefined;
			return async deliveredRoot => {
				const verdict = await runCodePhase(judge, deliveredRoot, signal);
				return verdict.ok
					? { ok: true as const }
					: { ok: false as const, evidence: `delivered tree failed ${judge.name}: ${verdict.summary}` };
			};
		};
		/** Writers whose change-sets are already on the shared root. */
		const landedPhases = new Set<string>();
		const engineOptions = {
			adwId,
			root: workRoot,
			workflow: workflow.name,
			traceDir,
			maxAttempts: workflow.maxAttempts,
			phases: workflow.phases.map(phase => ({
				// A fusion phase is one phase to the engine: it settles on the fuser's
				// envelope, and the panel is fan-out inside that single unit.
				name: phase.name,
				kind:
					phase.kind === "code"
						? TaskPhaseKind.Code
						: phase.kind === "human"
							? // The engine's own lane for work the caller performs and
								// reports back. Nothing here costs tokens.
								TaskPhaseKind.Engineer
							: TaskPhaseKind.Agent,
				owner: phase.kind === "fusion" ? (phase.fuser?.owner ?? "fusion") : (phase.owner ?? phase.kind),
				description: phase.description,
				// Resolved here, so the engine is handed a name and holds no policy
				// about which phase can fix a failure.
				dependsOn: phase.dependsOn,
				rewindTo: phase.onFail === "correct" ? rewindTarget(workflow, phase.name) : undefined,
				onReject: phase.onReject,
				inputs: phase.inputs,
			})),
			gates: workflow.phases
				.map(phase => ({ phase: phase.name, gates: phase.gates?.filter(gate => gate !== VERDICT_GATE) ?? [] }))
				.filter(phase => phase.gates.length > 0),
			undeclaredIgnore: workflow.undeclaredIgnore,
			claimsFile: path.join(runDir, "claims.json"),
		};
		// Resume rebuilds cursor, attempts and handoff from the trace; a fresh run
		// starts one.
		const run = resuming ? TaskRun.resume(engineOptions) : new TaskRun(engineOptions);
		traceRun = run;

		const artifactsDir = sessionArtifactsDir ?? runDir;
		const executeSeat = options.seatRunner ?? createExecutorSeatRunner({ host, workRoot, artifactsDir, signal });
		const seenSeats = new Set<string>();
		const spawnSeat: SeatRunner = seat => {
			// Revisited checks/reviews are new assignments, not format retries.
			// Reuse their sessions when possible, but deliver the current prompt.
			if (seenSeats.has(seat.id) && !seat.followUpMessage) seat.followUpMessage = seat.task;
			seenSeats.add(seat.id);
			return executeSeat(seat);
		};

		// Both writer paths submit through the same checks. Panel opinions never do.
		const submitWriter = (
			phaseName: string,
			execution: WriterExecution,
			guardReport?: TaskGuardReport,
			integrationNote?: string,
		): TaskOutcome => {
			if (guardReport?.unauthorized.length) {
				// Already reverted; the report is what turns the revert into a
				// rejection the writer can act on instead of a silent disappearance.
				run.noteGateReport(
					phaseName,
					"write_scope",
					guardReport.unauthorized.map(change => ({
						item: change.path,
						ok: false,
						note: `unauthorized ${change.kind} outside this phase's write scope; the change was reverted`,
					})),
				);
			}
			if (integrationNote) {
				run.noteGateReport(phaseName, "integration", [{ item: ".", ok: false, note: integrationNote }]);
			}
			if (execution.kind === "panel-failed") {
				return run.submitCodeResult(phaseName, false, execution.message);
			}
			const { seatName, role, result } = execution;
			run.notePhaseTokens(phaseName, seatName, result.tokens, result.model);
			if (result.exitCode !== 0) {
				const detail = result.stderr.trim() || "no stderr";
				// Whose failure was it? An out-of-memory kill or a provider rate
				// limit did not fail semantically, and the producer never got to
				// be wrong. Charging it a correction spends the budget on a
				// question nobody asked, and the next attempt would meet the
				// same exhausted machine.
				if (classifyFailure(detail) === "resource") {
					run.noteGateReport(phaseName, "resource", [
						{ item: seatName, ok: false, note: `host failure, not a fault in the work: ${detail}` },
					]);
					return run.haltResource(phaseName, `${role} ${seatName} could not run: ${detail}`);
				}
				// A crashed spawn has no complete answer; preserve ordinary retry behavior.
				return run.submitCodeResult(phaseName, false, `${role} ${seatName} exited ${result.exitCode}: ${detail}`);
			}
			const checked = phaseChecks.get(phaseName)?.(result.output);
			for (const report of checked?.reports ?? []) {
				run.noteGateReport(phaseName, report.gate, report.checks);
			}
			if (checked?.review) run.noteReviewDecision(phaseName, checked.review.approved, checked.review.reason);
			return run.submitAgentOutput(phaseName, result.output);
		};

		let haltReason = "";
		/** Opinions per fusion phase, retained across fuser retries only. */
		const panelCache = new Map<string, PanelOpinion[]>();

		/** Runs one agent or fusion attempt against `root`. No engine submission
		 * happens here beyond panel-opinion notes — the caller integrates and
		 * submits, which is what keeps concurrent completions serialized. */
		const executeWriterPhase = async (args: {
			phase: AdwPhaseConfig;
			step: TaskStep;
			root: string;
			phaseSignal: AbortSignal | undefined;
			handoff?: TaskHandoff | null;
			generation?: string;
			stepIndex: number;
		}): Promise<WriterExecution> => {
			const { phase, step, root, phaseSignal, stepIndex } = args;
			// A phase that declares `inputs` consumes exactly the outputs the engine
			// selected (and traced) for this dispatch; the incidental last envelope
			// is not context for it. Everything else keeps the positional handoff.
			const selectedInputs: TaskPhaseInput[] | undefined = phase.inputs ? (step.inputs ?? undefined) : undefined;
			const handoff: TaskHandoff | null = phase.inputs ? null : (args.handoff ?? run.handoff());
			// Record which instructions this attempt ran under, before it runs.
			// Prompts are files now, so two attempts of one phase can execute
			// under different text while the trace shows them as identical
			// dispatches — which makes the trace unable to answer the first
			// question asked of a behaviour change. The digest is enough to
			// compare attempts; git holds the text it names.
			if (phase.kind === "agent" || phase.kind === "fusion") {
				const contract = loadPrompt("envelope-contract", ENVELOPE_CONTRACT_BUNDLED, host.cwd);
				run.noteGateReport(phase.name, "prompt", [
					{ item: "envelope-contract", ok: true, note: describePromptOrigin("envelope-contract", contract) },
				]);
			}
			// Stable across attempts: the seat's session is continued on a retry,
			// so its id must not carry the attempt number.
			const stepId = `${adwId}-${phase.name}${args.generation ? `-${args.generation}` : ""}`;
			const correction = step.correction ?? undefined;

			if (phase.kind === "fusion") {
				const panel = phase.panel ?? [];
				const fuserSeat = phase.fuser;
				if (!fuserSeat)
					throw new Error(`Fusion phase "${phase.name}" lost its fuser between preflight and dispatch`);

				// Every seat answers the same question, independently: a seat that saw
				// another's answer would only agree with it. Concurrency honors the
				// operator's ceiling — a 6-seat panel under `task.maxConcurrency: 2`
				// would otherwise collect 429s that surface as phase failures.
				//
				// A retry re-runs the FUSER, not the panel: the opinions were not what
				// the gates rejected, and re-polling N models to receive the same
				// answers is the most expensive way to change nothing.
				const cached = panelCache.get(phase.name);
				// Built per seat: a seat carrying its own `prompt` is answering a
				// narrower question, so the prompt cannot be shared across the panel.
				const panelPromptFor = (seat: AdwSeatConfig) =>
					buildPanelPrompt({ request, phase, seat, handoff: handoff ?? undefined, inputs: selectedInputs });
				const limit = Math.max(1, host.settings?.get("task.maxConcurrency") ?? DEFAULT_MAX_PANEL_CONCURRENCY);
				const settled = cached
					? cached.map(opinion => ({ opinion, tokens: 0, cached: true }))
					: await mapWithLimit(panel, limit, async (seat, seatIndex) => {
							const base = roster.get(seat.owner);
							if (!base)
								throw new Error(`Panel seat "${seat.owner}" lost its agent between preflight and dispatch`);
							try {
								const outcome = await spawnSeat({
									seat: seat.owner,
									model: seat.model,
									thinking: seat.thinking,
									agent: derivePanelAgent(base),
									task: panelPromptFor(seat),
									id: `${stepId}-panel-${seatIndex}`,
									index: seatIndex,
									description: `adw ${workflow.name} · ${phase.name} · ${seat.owner}`,
									assignment: `${workflow.name}/${phase.name}#${seat.owner}`,
									readOnly: true,
									root,
									signal: phaseSignal,
								});
								const ok = outcome.exitCode === 0 && outcome.output.trim().length > 0;
								return {
									opinion: {
										owner: seat.owner,
										model: outcome.model,
										ok,
										text: ok ? outcome.output : outcome.stderr.trim() || "produced no answer",
									},
									tokens: outcome.tokens,
								};
							} catch (err) {
								// One dead seat must not kill the panel — the fuser is told it failed.
								return {
									opinion: {
										owner: seat.owner,
										model: seat.model ?? "default",
										ok: false,
										text: err instanceof Error ? err.message : String(err),
									},
									tokens: 0,
								};
							}
						});

				phaseSignal?.throwIfAborted();
				// A cached opinion was already traced on the attempt that produced it.
				for (const entry of settled) {
					if (!("cached" in entry)) {
						run.notePanelOpinion(
							phase.name,
							entry.opinion.owner,
							entry.opinion.ok,
							entry.tokens,
							entry.opinion.model,
						);
					}
				}
				const opinions: PanelOpinion[] = settled.map(entry => entry.opinion);
				panelCache.set(phase.name, opinions);

				if (!opinions.some(opinion => opinion.ok)) {
					return {
						kind: "panel-failed",
						message: `every panel seat failed: ${opinions.map(opinion => `${opinion.owner} (${opinion.text.split("\n")[0]})`).join("; ")}`,
					};
				}
				const fuserBase = roster.get(fuserSeat.owner);
				if (!fuserBase) throw new Error(`Fuser "${fuserSeat.owner}" lost its agent between preflight and dispatch`);
				const fused = await spawnSeat({
					seat: fuserSeat.owner,
					model: fuserSeat.model,
					thinking: fuserSeat.thinking,
					agent: deriveWriterAgent(fuserBase, host.cwd),
					task: buildFusionPrompt({
						request,
						phase,
						attempt: step.attempt,
						opinions,
						correction: step.correction ?? undefined,
						handoff: handoff ?? undefined,
						inputs: selectedInputs,
					}),
					id: `${stepId}-fuser`,
					index: stepIndex,
					description: `adw ${workflow.name} · ${phase.name} · fuse`,
					assignment: `${workflow.name}/${phase.name}`,
					readOnly: false,
					followUpMessage: correction,
					root,
					signal: phaseSignal,
				});
				return { kind: "seat", role: "fuser", seatName: fuserSeat.owner, result: fused };
			}

			const base = roster.get(phase.owner ?? "");
			if (!base) throw new Error(`Phase "${phase.name}" lost its agent between preflight and dispatch`);
			const spawned = await spawnSeat({
				seat: phase.owner ?? "",
				model: phase.model,
				thinking: phase.thinking,
				agent: deriveWriterAgent(base, host.cwd),
				task: buildPhasePrompt({
					request,
					phase,
					attempt: step.attempt,
					correction: step.correction ?? undefined,
					handoff: handoff ?? undefined,
					inputs: selectedInputs,
					// Only a phase gated on a verdict is judging a change-set.
					// Handing the diff to a builder would just be telling it
					// what it already wrote.
					reviewDiff: phase.gates?.includes(VERDICT_GATE) ? await collectReviewDiff(root, phaseSignal) : undefined,
				}),
				id: stepId,
				index: stepIndex,
				description: `adw ${workflow.name} · ${phase.name}`,
				assignment: `${workflow.name}/${phase.name}`,
				readOnly: false,
				followUpMessage: correction,
				root,
				signal: phaseSignal,
			});
			return { kind: "seat", role: "agent", seatName: base.name, result: spawned };
		};

		/** Structured inputs reach a command as a file, not argv: envelopes can
		 * exceed environment limits, and the file lives in the run dir so the
		 * working-tree diff gates never see it. */
		const buildCodeEnv = async (
			phase: AdwPhaseConfig,
			step: TaskStep,
		): Promise<Record<string, string> | undefined> => {
			const selectedInputs = phase.inputs ? (step.inputs ?? undefined) : undefined;
			if (!selectedInputs) return undefined;
			const inputsPath = path.join(runDir, "inputs", `${workflow.phases.indexOf(phase)}.json`);
			const byProducer = Object.fromEntries(
				selectedInputs.map(input => [
					input.phase,
					{
						version: input.version,
						summary: input.summary,
						artifacts: input.artifacts,
						notes_for_next_agent: input.notesForNextAgent,
						payload: JSON.parse(input.payloadJson) as unknown,
					},
				]),
			);
			await Bun.write(inputsPath, JSON.stringify(byProducer, null, "\t"));
			return { ADW_INPUTS: inputsPath };
		};

		/**
		 * The two lanes the caller executes and reports back: a deterministic
		 * command, and a human answering. Shared so the serial and concurrent
		 * schedulers cannot drift on what a `human` phase means — running one
		 * down the code path would execute a command it does not have.
		 */
		const runCallerPhase = async (phase: AdwPhaseConfig, step: TaskStep): Promise<TaskOutcome> => {
			if (phase.kind === "human") {
				const question = phase.description?.trim() || `Authorize phase "${phase.name}"?`;
				const answer = host.decideHuman
					? await host.decideHuman({ phase: phase.name, attempt: step.attempt, question })
					: // Fail closed: a host with no way to ask has not been told
						// yes, and inferring approval from silence is exactly what
						// a human gate exists to prevent.
						{ approved: false, reason: "no human decision channel is available to answer this phase" };
				return run.submitCodeResult(phase.name, answer.approved, answer.reason);
			}
			const env = await buildCodeEnv(phase, step);
			const result = await runCodePhase(phase, workRoot, signal, env);
			return run.submitCodeResult(phase.name, ...judgeExpectation(phase, result));
		};

		const finishRun = async (step: TaskStep): Promise<AdwRunResult> => {
			const acceptance = workflow.acceptance === "review" ? reviewAcceptance(run.handoff()) : undefined;
			let accepted = step.accepted && (acceptance?.accepted ?? true);
			let reason = step.accepted && acceptance ? acceptance.reason : haltReason;
			const applied = isolation ? await settleIsolation(isolation, accepted, runDir, adwId) : undefined;
			if (applied?.conflict) {
				accepted = false;
				reason = applied.conflict;
			}
			const result = run.finish(accepted, reason);
			completed = true;
			logger.debug("adw run finished", { adwId, workflow: workflow.name, accepted, reason, traceDir });
			return { adwId, summary: result, traceDir, isolation: applied };
		};

		const reportOutcome = (phase: AdwPhaseConfig, spec: TaskPhaseSpec, step: TaskStep, outcome: TaskOutcome) => {
			if (outcome.kind === TaskOutcomeKind.Aborted) haltReason = outcome.reason ?? "";
			if (outcome.kind !== TaskOutcomeKind.Retry || outcome.phase !== phase.name) panelCache.delete(phase.name);
			onPhase?.({
				phase: phase.name,
				owner: spec.owner,
				kind: phase.kind,
				attempt: step.attempt,
				outcome:
					outcome.kind === TaskOutcomeKind.Advanced
						? "advanced"
						: outcome.kind === TaskOutcomeKind.Retry
							? "retry"
							: "aborted",
				violations: outcome.correction ? [outcome.correction] : outcome.reason ? [outcome.reason] : undefined,
			});
		};

		if (concurrency === 1) {
			const baselineFile = path.join(runDir, "baseline.json");
			guard = TaskWriteGuard.create({ root: workRoot, baselineFile });
		}
		if (concurrency === 1) {
			// The serial path: one phase in flight, executed against the run root,
			// guarded by the run-level write guard exactly as before concurrency
			// existed. `Wait` cannot occur with a single flight.
			for (let stepIndex = 0; ; stepIndex++) {
				signal?.throwIfAborted();
				const step = run.nextStep();
				if (step.kind === TaskStepKind.Done) return await finishRun(step);
				const spec = step.phase;
				if (!spec) throw new Error(`Engine returned a Run step with no phase for workflow "${workflow.name}"`);
				const phase = phasesByName.get(spec.name);
				if (!phase) throw new Error(`Engine returned unknown phase "${spec.name}"`);
				onPhase?.({ phase: phase.name, owner: spec.owner, kind: phase.kind, attempt: step.attempt });

				let outcome: TaskOutcome;
				if (phase.kind === "human" || phase.kind === "code") {
					outcome = await runCallerPhase(phase, step);
				} else {
					if (
						resuming &&
						stepIndex === 0 &&
						(await Bun.file(path.join(runDir, "baseline.json.attempt")).exists())
					) {
						requireRecovered(settleGuard(phase));
					}
					guard?.begin();
					guardBoundary = phase;
					const execution = await executeWriterPhase({
						phase,
						step,
						root: workRoot,
						phaseSignal: signal,
						stepIndex,
					});
					const guardReport = guard ? settleGuard(phase) : undefined;
					requireRecovered(guardReport);
					outcome = submitWriter(phase.name, execution, guardReport);
				}
				reportOutcome(phase, spec, step, outcome);
			}
		}

		// Writer turns overlap; cloning, judgment, integration and code barriers
		// have one owner. Nothing rejected by a gate reaches the integration root.
		const wsRepoRoot = await getRepoRoot(workRoot);
		const wsBackend = parseIsolationBackend(host.settings?.get("isolation.backend") ?? "auto");
		interface WorkspaceRecord {
			handle: IsolationHandle;
			context: IsolationContext;
			generation: string;
			fromSeq: number;
			retired?: boolean;
		}
		interface Workspace extends WorkspaceRecord {
			guard: TaskWriteGuard;
			dir: string;
			baselineFile: string;
		}
		interface Flight {
			phase: AdwPhaseConfig;
			spec: TaskPhaseSpec;
			step: TaskStep;
			ws: Workspace;
			controller: AbortController;
			promise: Promise<WriterExecution | { kind: "crashed"; error: unknown }>;
		}
		const inFlight = new Map<string, Flight>();
		const workspaces = new Map<string, Workspace>();
		let dispatchHalted = false;
		const workspaceDir = (phase: AdwPhaseConfig) =>
			path.join(runDir, "workspaces", String(workflow.phases.indexOf(phase)));
		const settleWorkspace = (phase: AdwPhaseConfig, ws: Workspace) => {
			const report = ws.guard.settle({
				allowed: phase.writes,
				protectedGlobs: workflow.protected ?? [],
				patchDir: ws.dir,
			});
			requireRecovered(report);
			return report;
		};
		const dropWorkspace = async (name: string) => {
			const ws = workspaces.get(name);
			if (!ws) return;
			await writeRunState(path.join(ws.dir, "workspace.json"), {
				handle: ws.handle,
				context: ws.context,
				generation: ws.generation,
				fromSeq: ws.fromSeq,
				retired: true,
			});
			await cleanupIsolation(ws.handle);
			workspaces.delete(name);
			await fs.rm(path.join(ws.dir, "workspace.json"), { force: true });
		};
		const ensureWorkspace = async (phase: AdwPhaseConfig): Promise<Workspace> => {
			const existing = workspaces.get(phase.name);
			if (existing) return existing;
			const dir = workspaceDir(phase);
			const recordFile = path.join(dir, "workspace.json");
			let record: WorkspaceRecord | undefined;
			try {
				record = (await Bun.file(recordFile).json()) as WorkspaceRecord;
			} catch (error) {
				if (!isEnoent(error)) throw error;
			}
			if (record) {
				const reader = new TaskTraceReader(traceDir);
				const raw = reader.readRaw(record.fromSeq);
				const strings = reader.strings();
				const layout = taskTraceLayout();
				for (let at = 0; at + layout.recordLen <= raw.length; at += layout.recordLen) {
					if (strings[raw.readUInt32LE(at + layout.offPhase) - 1] !== phase.name) continue;
					const kind = layout.kindNames[raw[at + layout.offKind] as number];
					if (
						(kind === "phase_invalidated" && raw.readUInt32LE(at + layout.offOwner) !== 0) ||
						(kind === "phase_finished" && ((raw[at + layout.offFlags] as number) & layout.flagOk) !== 0)
					) {
						record.retired = true;
					}
				}
				if (record.retired) {
					await cleanupIsolation(record.handle);
					record = undefined;
				}
			}
			if (!record) {
				const generation = String(Snowflake.next());
				// Await cloning here, not inside the turn promise: another accepted
				// patch cannot mutate the source halfway through a snapshot.
				const handle = await ensureIsolation(wsRepoRoot, `${adwId}-${generation}`, wsBackend);
				const context = await prepareIsolationContext(handle.mergedDir);
				record = { handle, context, generation, fromSeq: new TaskTraceReader(traceDir).count() };
				await writeRunState(recordFile, record);
			} else {
				await fs.access(record.handle.mergedDir);
				await writeIsolationOwner(path.dirname(record.handle.mergedDir), adwId);
			}
			const baselineFile = path.join(dir, `${record.generation}.baseline.json`);
			const ws: Workspace = {
				...record,
				dir,
				baselineFile,
				guard: TaskWriteGuard.create({ root: record.handle.mergedDir, baselineFile }),
			};
			workspaces.set(phase.name, ws);
			if (await Bun.file(`${baselineFile}.attempt`).exists()) settleWorkspace(phase, ws);
			return ws;
		};
		let pendingCode: { phase: AdwPhaseConfig; spec: TaskPhaseSpec; step: TaskStep } | null = null;
		const invalidate = async (outcome: TaskOutcome) => {
			const names = outcome.invalidated ?? [];
			for (const name of names) {
				panelCache.delete(name);
				inFlight.get(name)?.controller.abort(new Error(`phase "${name}" was invalidated`));
				if (pendingCode?.phase.name === name) pendingCode = null;
			}
			// Drain the old generation before either deleting its tree or asking
			// the engine to dispatch its replacement; late results are evidence,
			// never submissions against a new Running state of the same name.
			for (const name of names) {
				const stale = inFlight.get(name);
				if (stale) {
					await stale.promise;
					settleWorkspace(stale.phase, stale.ws);
					await preserveDelta(
						path.join(stale.ws.dir, "invalidated", stale.ws.generation),
						await captureDeltaPatch(stale.ws.handle.mergedDir, stale.ws.context.baseline),
					);
					inFlight.delete(name);
				}
				await dropWorkspace(name);
			}
		};
		try {
			for (let stepIndex = 0; ; stepIndex++) {
				signal?.throwIfAborted();
				while (!dispatchHalted && !pendingCode && inFlight.size < concurrency) {
					const step = run.nextStep();
					if (step.kind === TaskStepKind.Done) {
						if (inFlight.size === 0) return await finishRun(step);
						break;
					}
					if (step.kind === TaskStepKind.Wait) break;
					const spec = step.phase;
					if (!spec) throw new Error("engine returned a Run step without a phase");
					const phase = phasesByName.get(spec.name);
					if (!phase) throw new Error(`engine returned unknown phase "${spec.name}"`);
					if (phase.kind === "code" || phase.kind === "human") {
						pendingCode = { phase, spec, step };
						break;
					}
					const handoff = run.handoff();
					const ws = await ensureWorkspace(phase);
					ws.guard.begin();
					run.setPhaseRoot(phase.name, ws.handle.mergedDir);
					const controller = new AbortController();
					const phaseSignal = signal ? AbortSignal.any([signal, controller.signal]) : controller.signal;
					const promise = executeWriterPhase({
						phase,
						step,
						root: ws.handle.mergedDir,
						phaseSignal,
						stepIndex,
						handoff,
						generation: ws.generation,
					}).catch((error: unknown) => ({ kind: "crashed" as const, error }));
					inFlight.set(spec.name, { phase, spec, step, ws, controller, promise });
					onPhase?.({ phase: phase.name, owner: spec.owner, kind: phase.kind, attempt: step.attempt });
				}
				if (dispatchHalted && inFlight.size === 0) {
					return await finishRun({ kind: TaskStepKind.Done, accepted: false, attempt: 0 });
				}
				if (pendingCode && inFlight.size === 0) {
					const { phase, spec, step } = pendingCode;
					pendingCode = null;
					onPhase?.({ phase: phase.name, owner: spec.owner, kind: phase.kind, attempt: step.attempt });
					const outcome = await runCallerPhase(phase, step);
					await invalidate(outcome);
					reportOutcome(phase, spec, step, outcome);
					continue;
				}
				if (inFlight.size === 0) throw new Error("DAG scheduler is waiting without an active phase");
				const flight = await Promise.race(
					[...inFlight.values()].map(async active => {
						await active.promise;
						return active;
					}),
				);
				const execution = await flight.promise;
				const report = settleWorkspace(flight.phase, flight.ws);
				const delta = await captureDeltaPatch(flight.ws.handle.mergedDir, flight.ws.context.baseline);
				const fromSeq = new TaskTraceReader(traceDir).count();
				await preserveDelta(path.join(flight.ws.dir, "history", String(fromSeq)), delta);
				const record: IntegrationRecord = {
					phase: flight.phase.name,
					fromSeq,
					delta,
					status: "prepared",
				};
				await writeRunState(path.join(runDir, "integration.json"), record);
				const outcome =
					execution.kind === "crashed"
						? run.submitCodeResult(
								flight.phase.name,
								false,
								execution.error instanceof Error ? execution.error.message : String(execution.error),
							)
						: submitWriter(flight.phase.name, execution, report);
				inFlight.delete(flight.phase.name);
				if (outcome.kind === TaskOutcomeKind.Advanced && !dispatchHalted) {
					try {
						// The candidate was verified inside the writer's own
						// workspace; this landing goes to the shared root.
						// Re-run the guard phase there, so a change-set that is
						// valid alone and broken in company is caught before it
						// is called delivered.
						await integrateAccepted(workRoot, runDir, record, deliveredCheckFor(flight.phase, landedPhases));
						landedPhases.add(flight.phase.name);
					} catch (error) {
						dispatchHalted = true;
						haltReason = error instanceof Error ? error.message : String(error);
					}
				} else {
					record.status = "rejected";
					await writeRunState(path.join(runDir, "integration.json"), record);
				}
				await invalidate(outcome);
				const retriesHere = outcome.kind === TaskOutcomeKind.Retry && outcome.phase === flight.phase.name;
				if (!retriesHere) await dropWorkspace(flight.phase.name);
				reportOutcome(flight.phase, flight.spec, flight.step, outcome);
			}
		} finally {
			for (const flight of inFlight.values()) flight.controller.abort(new Error("the run is stopping"));
			for (const flight of inFlight.values()) {
				await flight.promise;
				settleWorkspace(flight.phase, flight.ws);
				await preserveDelta(
					path.join(flight.ws.dir, "interrupted", flight.ws.generation),
					await captureDeltaPatch(flight.ws.handle.mergedDir, flight.ws.context.baseline),
				);
			}
			// A thrown interruption is resumable: keep private trees and manifests.
			// A decided result has its patches and trace, so release all loaded trees.
			if (completed) for (const name of workspaces.keys()) await dropWorkspace(name);
		}
	} catch (err) {
		const reason = err instanceof Error ? err.message : String(err);
		// Restore what the dying attempt was not allowed to change before the
		// tree state is captured anywhere else; the patch below must reflect it.
		if (guardBoundary) {
			try {
				requireRecovered(settleGuard(guardBoundary));
			} catch (guardErr) {
				logger.warn("adw write guard could not settle after failure", { adwId, error: guardErr });
			}
		}
		try {
			traceRun?.finish(false, reason);
		} catch (finishErr) {
			// The trace write itself failed; the original cause still wins.
			logger.warn("adw could not close the trace", { adwId, error: finishErr });
		}
		// A died run's work is still on disk in the sandbox; preserve it as a
		// patch before the sandbox goes away in `finally`.
		if (isolation && concurrency === 1) {
			try {
				await settleIsolation(isolation, false, runDir, adwId);
			} catch (patchErr) {
				logger.warn("adw could not preserve the sandbox diff", { adwId, error: patchErr });
			}
		}
		throw new AdwRunError(reason, adwId, traceDir, { cause: err });
	} finally {
		// The sandbox is a materialised copy of the repo; leaking one per run
		// fills the worktree dir with abandoned checkouts.
		if (isolation && (completed || concurrency === 1)) await cleanupIsolation(isolation.handle);
	}
}
