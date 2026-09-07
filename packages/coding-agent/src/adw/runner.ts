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
	type TaskOutcome,
	TaskOutcomeKind,
	TaskPhaseKind,
	TaskRun,
	type TaskRunSummary,
	TaskStepKind,
} from "@oh-my-pi/pi-natives";
import { getAgentDir, logger, ptree, Snowflake } from "@oh-my-pi/pi-utils";
import { executeShell } from "@oh-my-pi/pi-natives";
import { registerArtifactsDir } from "../internal-urls/registry-helpers";
import type { LocalProtocolOptions } from "../internal-urls/local-protocol";
import type { ArtifactManager } from "../session/artifacts";
import type { EventBus } from "../utils/event-bus";
import type { ModelRegistry } from "../config/model-registry";
import { resolveAgentModelSelection } from "../config/model-resolver";
import type { Settings } from "../config/settings";
import type { AuthStorage } from "../session/auth-storage";
import { discoverAgents, getAgent } from "../task/discovery";
import { type ExecutorOptions, runSubagentFollowUpTurn, runSubprocess } from "../task/executor";
import type { AgentDefinition, SingleResult } from "../task/types";
import { parseConfiguredThinkingLevel } from "../thinking";
import { type IsolationContext, prepareIsolationContext } from "../task/isolation-runner";
import {
	applyNestedPatches,
	captureDeltaPatch,
	cleanupIsolation,
	ensureIsolation,
	type IsolationHandle,
	parseIsolationBackend,
} from "../task/worktree";
import * as vcs from "@oh-my-pi/pi-natives/vcs";
import { buildFusionPrompt, buildPanelPrompt, buildPhasePrompt, ENVELOPE_CONTRACT, type PanelOpinion } from "./prompt";
import type { AdwPhaseConfig, AdwPhaseProgress, AdwWorkflowConfig } from "./types";

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
 * Nested repositories (submodules, vendored checkouts) carry their own diffs
 * and are applied after the root, and only if the root landed: a nested commit
 * on top of a root that failed to apply would leave the tree inconsistent.
 */
export async function settleIsolation(
	isolation: { handle: IsolationHandle; context: IsolationContext },
	accepted: boolean,
	runDir: string,
	adwId: string,
): Promise<AdwIsolationOutcome> {
	const { handle, context } = isolation;
	const delta = await captureDeltaPatch(handle.mergedDir, context.baseline);
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
	if (!accepted) return { ...base, patchPath };

	if (base.hadChanges) {
		const repo = vcs.requireGit(context.repoRoot);
		if (!(await repo.canApplyPatch(text, {}).catch(() => false))) {
			return { ...base, patchPath, conflict: "the parent checkout moved under the run; patch preserved" };
		}
		await repo.applyPatch(text, {});
	}

	if (delta.nestedPatches.length === 0) return { ...base, applied: true, patchPath };
	// Non-fatal: the root already landed, and a failed submodule apply must not
	// retroactively turn an accepted run into a failure. It gets named instead.
	const nestedWarnings = await applyNestedPatches(context.repoRoot, delta.nestedPatches).catch((err: unknown) => [
		`nested repository patches failed to apply: ${err instanceof Error ? err.message : String(err)}`,
	]);
	return { ...base, applied: true, patchPath, nestedApplied: true, nestedWarnings };
}

/** The signals a workflow command realistically dies by, named for the report. */
const SIGNAL_NAMES: Record<number, string> = {
	1: "SIGHUP",
	2: "SIGINT",
	9: "SIGKILL",
	13: "SIGPIPE",
	15: "SIGTERM",
};

export async function runCodePhase(
	phase: AdwPhaseConfig,
	cwd: string,
	signal: AbortSignal | undefined,
): Promise<{ ok: boolean; summary: string }> {
	const command = phase.command ?? "";
	const timeout = phase.timeoutMs ?? DEFAULT_COMMAND_TIMEOUT_MS;
	try {
		// Absolute shell on POSIX (a launcher may hand omp a minimal tool-only
		// PATH); the vendored Brush shell on Windows, where /bin/sh does not
		// exist. Same split as runShellCommand in config/resolve-config-value.ts.
		if (process.platform === "win32") {
			let output = "";
			const result = await executeShell({ command, cwd, timeoutMs: timeout, signal }, (err, chunk) => {
				if (!err) output += chunk;
			});
			if (result.timedOut)
				return { ok: false, summary: `${command} timed out after ${timeout}ms\n${output.trim()}` };
			const ok = result.exitCode === 0;
			return {
				ok,
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
			return { ok: false, summary: `${command} ${why}\n${summary}` };
		}
		if (result.exitCode !== null && result.exitCode > 128) {
			// `sh` reports a child that died by signal N as `128 + N`. Left raw, a
			// host shutting down (SIGTERM ⇒ 143) reads to the next agent as a
			// failing build, and it spends its attempts fixing nothing.
			const signalNumber = result.exitCode - 128;
			const name = SIGNAL_NAMES[signalNumber] ?? `signal ${signalNumber}`;
			return { ok: false, summary: `${command} was killed by ${name}\n${summary}` };
		}
		return {
			ok: result.exitCode === 0,
			summary:
				result.exitCode === 0
					? summary || `${command} exited 0`
					: `${command} exited ${result.exitCode}\n${summary}`,
		};
	} catch (err) {
		return { ok: false, summary: `${command} could not run: ${err instanceof Error ? err.message : String(err)}` };
	}
}

/** A writer seat: the agent's own prompt plus the envelope contract it must satisfy. */
function deriveWriterAgent(base: AgentDefinition): AgentDefinition {
	return { ...base, systemPrompt: `${base.systemPrompt}\n\n${ENVELOPE_CONTRACT}` };
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
	 * Set on a retry: the correction to continue the seat's existing session
	 * with, rather than the full prompt of a fresh spawn.
	 */
	followUpMessage?: string;
}

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
			tokens: result.tokens,
			model: result.resolvedModel ?? patterns[0] ?? seat.agent.model?.[0] ?? "default",
		});

		if (seat.followUpMessage) {
			try {
				return toOutcome(
					await runSubagentFollowUpTurn({
						id: seat.id,
						agent: seat.agent,
						message: seat.followUpMessage,
						index: seat.index,
						description: seat.description,
						modelRole: role,
						signal,
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
					workRoot,
					artifactsDir,
					modelPatterns: patterns,
					modelRole: role,
					signal,
				}),
			),
		);
	};
}

export async function runAdw(options: AdwRunOptions): Promise<AdwRunResult> {
	const { host, workflow, signal, onPhase } = options;
	const resuming = options.resumeAdwId;
	if (resuming && workflow.isolation) {
		// Checked before any I/O: the sandbox was torn down when the run died and
		// its uncommitted work went with it, so resuming into a fresh sandbox
		// would silently restart the completed phases from the base commit. This
		// is knowable from the config alone, so it must not surface as a missing
		// file from somewhere deeper.
		throw new Error(
			`Workflow "${workflow.name}" runs isolated, and an isolated run cannot be resumed — its sandbox is gone. Its diff was preserved as a patch; re-run instead.`,
		);
	}
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

	// One sandbox for the WHOLE workflow, not one per spawn: `plan` writes files
	// that `build` reads and that gates verify, so per-spawn isolation would
	// throw away the continuity the phases depend on. Changes reach the real
	// checkout once, at the end, and only if the run was accepted.
	let isolation: { handle: IsolationHandle; context: IsolationContext } | undefined;
	if (workflow.isolation && resuming) {
		// The sandbox was torn down when the run died, and its uncommitted work
		// went with it. Resuming into a fresh sandbox would silently restart the
		// completed phases' file changes from the base commit.
		throw new Error(
			`Workflow "${workflow.name}" runs isolated, and an isolated run cannot be resumed — its sandbox is gone. Its diff was preserved as a patch; re-run instead.`,
		);
	}
	if (workflow.isolation) {
		const context = await prepareIsolationContext(host.cwd);
		const backend = parseIsolationBackend(host.settings?.get("isolation.backend") ?? "auto");
		const handle = await ensureIsolation(context.repoRoot, adwId, backend);
		isolation = { handle, context };
		logger.debug("adw isolated", { adwId, backend: handle.backend, dir: handle.mergedDir });
	}
	/** Where phases actually run. The roster is still discovered from the real cwd. */
	const workRoot = isolation?.handle.mergedDir ?? host.cwd;

	const phasesByName = new Map(workflow.phases.map(phase => [phase.name, phase]));
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
			kind: phase.kind === "code" ? TaskPhaseKind.Code : TaskPhaseKind.Agent,
			owner: phase.kind === "fusion" ? (phase.fuser?.owner ?? "fusion") : (phase.owner ?? phase.kind),
			description: phase.description,
		})),
		gates: workflow.phases
			.filter(phase => (phase.gates?.length ?? 0) > 0)
			.map(phase => ({ phase: phase.name, gates: phase.gates ?? [] })),
	};
	// Resume rebuilds cursor, attempts and handoff from the trace; a fresh run
	// starts one.
	const run = resuming ? TaskRun.resume(engineOptions) : new TaskRun(engineOptions);

	const artifactsDir = sessionArtifactsDir ?? runDir;
	const spawnSeat = options.seatRunner ?? createExecutorSeatRunner({ host, workRoot, artifactsDir, signal });

	// The engine halts on its own; this only stops a bug from spinning forever.
	const maxSteps = workflow.phases.length * run.maxAttempts + 8;
	let haltReason = "";
	/** Opinions for the phase currently being attempted; dropped once it settles. */
	let panelCache: { phase: string; opinions: PanelOpinion[] } | null = null;

	// Every exit path must reach `finish()`: it writes the only terminal record
	// in events.bin, and without it a reader cannot tell a dead run from a
	// running one. The catch also hands the caller the trace it already paid for.
	try {
		for (let stepIndex = 0; stepIndex < maxSteps; stepIndex++) {
			signal?.throwIfAborted();
			const step = run.nextStep();
			if (step.kind === TaskStepKind.Done) {
				const result = run.finish(step.accepted, haltReason);
				const applied = isolation ? await settleIsolation(isolation, step.accepted, runDir, adwId) : undefined;
				logger.debug("adw run finished", { adwId, workflow: workflow.name, accepted: step.accepted, traceDir });
				return { adwId, summary: result, traceDir, isolation: applied };
			}

			const spec = step.phase;
			if (!spec) throw new Error(`Engine returned a Run step with no phase for workflow "${workflow.name}"`);
			const phase = phasesByName.get(spec.name);
			if (!phase) throw new Error(`Engine returned unknown phase "${spec.name}"`);

			onPhase?.({ phase: phase.name, owner: spec.owner, kind: phase.kind, attempt: step.attempt });
			const handoff: TaskHandoff | null = run.handoff();
			// Stable across attempts: the seat's session is continued on a retry,
			// so its id must not carry the attempt number.
			const stepId = `${adwId}-${phase.name}`;
			const correction = step.attempt > 1 ? (step.correction ?? undefined) : undefined;

			let outcome: TaskOutcome;
			if (phase.kind === "code") {
				const { ok, summary } = await runCodePhase(phase, workRoot, signal);
				outcome = run.submitCodeResult(ok, summary);
			} else if (phase.kind === "fusion") {
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
				const cached = panelCache?.phase === phase.name ? panelCache.opinions : undefined;
				const panelPrompt = buildPanelPrompt({ request, phase, handoff: handoff ?? undefined });
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
									task: panelPrompt,
									id: `${stepId}-panel-${seatIndex}`,
									index: seatIndex,
									description: `adw ${workflow.name} · ${phase.name} · ${seat.owner}`,
									assignment: `${workflow.name}/${phase.name}#${seat.owner}`,
									readOnly: true,
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

				// A cached opinion was already traced on the attempt that produced it.
				for (const entry of settled) {
					if (!("cached" in entry)) run.notePanelOpinion(entry.opinion.owner, entry.opinion.ok, entry.tokens);
				}
				const opinions: PanelOpinion[] = settled.map(entry => entry.opinion);
				panelCache = { phase: phase.name, opinions };

				if (!opinions.some(opinion => opinion.ok)) {
					outcome = run.submitCodeResult(
						false,
						`every panel seat failed: ${opinions.map(opinion => `${opinion.owner} (${opinion.text.split("\n")[0]})`).join("; ")}`,
					);
				} else {
					const fuserBase = roster.get(fuserSeat.owner);
					if (!fuserBase)
						throw new Error(`Fuser "${fuserSeat.owner}" lost its agent between preflight and dispatch`);
					const fused = await spawnSeat({
						seat: fuserSeat.owner,
						model: fuserSeat.model,
						thinking: fuserSeat.thinking,
						agent: deriveWriterAgent(fuserBase),
						task: buildFusionPrompt({
							request,
							phase,
							attempt: step.attempt,
							opinions,
							correction: step.correction ?? undefined,
							handoff: handoff ?? undefined,
						}),
						id: `${stepId}-fuser`,
						index: stepIndex,
						description: `adw ${workflow.name} · ${phase.name} · fuse`,
						assignment: `${workflow.name}/${phase.name}`,
						readOnly: false,
						followUpMessage: correction,
					});
					// Charged before the verdict: a crashed or rejected attempt spent
					// these tokens too, and only this layer knows what they were.
					run.notePhaseTokens(fuserSeat.owner, fused.tokens);
					outcome =
						fused.exitCode === 0
							? run.submitAgentOutput(fused.output)
							: run.submitCodeResult(
									false,
									`fuser ${fuserSeat.owner} exited ${fused.exitCode}: ${fused.stderr.trim() || "no stderr"}`,
								);
				}
			} else {
				const base = roster.get(phase.owner ?? "");
				if (!base) throw new Error(`Phase "${phase.name}" lost its agent between preflight and dispatch`);
				const spawned = await spawnSeat({
					seat: phase.owner ?? "",
					model: phase.model,
					thinking: phase.thinking,
					agent: deriveWriterAgent(base),
					task: buildPhasePrompt({
						request,
						phase,
						attempt: step.attempt,
						correction: step.correction ?? undefined,
						handoff: handoff ?? undefined,
					}),
					id: stepId,
					index: stepIndex,
					description: `adw ${workflow.name} · ${phase.name}`,
					assignment: `${workflow.name}/${phase.name}`,
					readOnly: false,
					followUpMessage: correction,
				});
				run.notePhaseTokens(base.name, spawned.tokens);
				outcome =
					spawned.exitCode === 0
						? run.submitAgentOutput(spawned.output)
						: // A crashed spawn produced no answer at all. Reporting it as a
							// failed result keeps the retry path identical to a red gate
							// instead of feeding the engine a truncated turn to parse.
							run.submitCodeResult(
								false,
								`agent ${base.name} exited ${spawned.exitCode}: ${spawned.stderr.trim() || "no stderr"}`,
							);
			}

			if (outcome.kind === TaskOutcomeKind.Aborted) haltReason = outcome.reason ?? "";
			if (outcome.kind !== TaskOutcomeKind.Retry) panelCache = null;
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
		}
		throw new Error(`Workflow "${workflow.name}" exceeded ${maxSteps} steps without settling`);
	} catch (err) {
		const reason = err instanceof Error ? err.message : String(err);
		try {
			run.finish(false, reason);
		} catch (finishErr) {
			// The trace write itself failed; the original cause still wins.
			logger.warn("adw could not close the trace", { adwId, error: finishErr });
		}
		// A died run's work is still on disk in the sandbox; preserve it as a
		// patch before the sandbox goes away in `finally`.
		if (isolation) {
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
		if (isolation) await cleanupIsolation(isolation.handle);
	}
}
