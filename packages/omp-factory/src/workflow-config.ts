/**
 * Load the frozen ADW workflow format into graph phases.
 *
 * The declarative contract is the prototype's, so an existing `.omp/adw/*.yml`
 * runs here without being rewritten. Anything this runner cannot honour is
 * refused at load time rather than silently dropped — a workflow that parses
 * must mean what it says.
 */
import { type BaselineState, captureBaselineState } from "./capture";
import type { GraphPhase, SelectedInput } from "./graph";
import type { Candidate } from "./loop";
import type { ReviewVerdict } from "./review";

/** One panel opinion, labelled by the seat that produced it. */
export interface PanelOpinion {
	seat: string;
	owner: string;
	text: string;
	failed: boolean;
}

export interface AgentRunContext {
	phase: string;
	owner: string;
	/** Seat-pinned model pattern, when the workflow named one. */
	model?: string;
	/** Seat-pinned thinking level, when the workflow named one. */
	thinking?: string;
	prompt: string;
	attempt: number;
	correction: string | undefined;
	inputs: SelectedInput[];
	workspace: string;
	/**
	 * The tree as the phase entered it, resolved on first use and pinned for
	 * the rest of the dispatch. Capturing per attempt instead lets a rejected
	 * attempt launder its own violations: attempt 2's change-set would be
	 * measured after attempt 1's writes, so the protected file attempt 1 was
	 * rejected for is invisible to the check that would catch it. Lazy
	 * because change capture needs a git repo and a caller that does its own
	 * accounting should not be made to have one.
	 */
	entryState: () => Promise<BaselineState>;
	/**
	 * Panel seats investigate and never write; the caller must restrict the
	 * seat's tools accordingly. Exactly one seat per fusion phase — the
	 * fuser — receives `readOnly: false`.
	 */
	readOnly: boolean;
	/** Panel opinions handed to a fuser, labelled and in declared order. */
	opinions?: PanelOpinion[];
}

export interface WorkflowConfigOptions {
	/** Runs one agent phase; production wires this to a managed subagent. */
	runAgent: (context: AgentRunContext) => Promise<Candidate>;
	/** Runs a `code` phase command; defaults to the deterministic gate runner. */
	review?: (phase: string, candidate: Candidate) => Promise<ReviewVerdict>;
	/** Operator request, appended to each agent prompt. */
	request?: string;
}

export interface LoadedWorkflow {
	name: string;
	description?: string;
	/** `isolation: true` — the caller supplies the provider. */
	isolation: boolean;
	acceptance: "phases" | "review";
	protectedGlobs: string[];
	phases: GraphPhase[];
}

const SUPPORTED_GATES = new Set([
	"artifacts_exist",
	"files_non_empty",
	"json_parses",
	"diff_matches_claims",
	"verdict_consistent",
]);
/** `.omp/adw/**` is protected in the prototype whatever the file says. */
const ALWAYS_PROTECTED = [".omp/adw/**"];

interface RawPhase {
	name?: unknown;
	kind?: unknown;
	owner?: unknown;
	model?: unknown;
	thinking?: unknown;
	description?: unknown;
	prompt?: unknown;
	command?: unknown;
	gates?: unknown;
	dependsOn?: unknown;
	inputs?: unknown;
	writes?: unknown;
	onFail?: unknown;
	onReject?: unknown;
	timeoutMs?: unknown;
	panel?: unknown;
	fuser?: unknown;
}

interface RawWorkflow {
	name?: unknown;
	description?: unknown;
	maxAttempts?: unknown;
	acceptance?: unknown;
	isolation?: unknown;
	protected?: unknown;
	undeclaredIgnore?: unknown;
	phases?: unknown;
}

const WORKFLOW_KEYS = new Set<string>([
	"name",
	"description",
	"maxAttempts",
	"acceptance",
	"isolation",
	"protected",
	"undeclaredIgnore",
	"phases",
]);
const PHASE_KEYS = new Set<string>([
	"name",
	"kind",
	"owner",
	"model",
	"thinking",
	"description",
	"prompt",
	"command",
	"gates",
	"dependsOn",
	"inputs",
	"writes",
	"onFail",
	"onReject",
	"timeoutMs",
	"panel",
	"fuser",
]);
const SEAT_KEYS = new Set<string>(["owner", "model", "thinking"]);

/**
 * A misspelled key is not a harmless no-op: `isolaton: true` would run the
 * whole workflow against the real checkout while the operator believes it is
 * sandboxed. Anything this runner does not implement is named and refused.
 */
function assertKnownKeys(value: object, known: Set<string>, label: string): void {
	const surplus = Object.keys(value).filter(key => !known.has(key));
	if (surplus.length > 0) {
		throw new Error(`${label} has keys this runner does not implement: ${surplus.join(", ")}`);
	}
}

function stringList(value: unknown, label: string): string[] {
	if (value === undefined) return [];
	if (!Array.isArray(value)) throw new Error(`${label} must be a list`);
	return value.map(entry => {
		if (typeof entry !== "string" || entry.trim().length === 0) throw new Error(`${label} entries must be strings`);
		return entry;
	});
}

interface RawSeat {
	owner?: unknown;
	model?: unknown;
	thinking?: unknown;
}

/**
 * Read one seat's identity. `model`/`thinking` are pinned per seat because a
 * panel whose seats resolve to the same model reports two opinions when it
 * holds one.
 */
function readSeat(seat: RawSeat, label: string): { owner: string; model?: string; thinking?: string } {
	assertKnownKeys(seat, SEAT_KEYS, label);
	if (seat.owner !== undefined && typeof seat.owner !== "string") throw new Error(`${label} has a non-string owner`);
	if (seat.model !== undefined && typeof seat.model !== "string") throw new Error(`${label} has a non-string model`);
	if (seat.thinking !== undefined && typeof seat.thinking !== "string") {
		throw new Error(`${label} has a non-string thinking level`);
	}
	return {
		owner: typeof seat.owner === "string" ? seat.owner : "task",
		model: typeof seat.model === "string" ? seat.model : undefined,
		thinking: typeof seat.thinking === "string" ? seat.thinking : undefined,
	};
}

/**
 * Nearest ancestor that owns a writer — the only phase that can act on a
 * correction. `fusion` counts: it writes, so a workflow whose only writer is
 * a panel can still be corrected.
 */
function nearestWriterAncestor(kinds: Map<string, string>, deps: Map<string, string[]>, from: string): string | null {
	const queue = [...(deps.get(from) ?? [])];
	const seen = new Set<string>();
	while (queue.length > 0) {
		const current = queue.shift();
		if (current === undefined || seen.has(current)) continue;
		seen.add(current);
		const kind = kinds.get(current);
		if (kind === "agent" || kind === "fusion") return current;
		queue.push(...(deps.get(current) ?? []));
	}
	return null;
}

export function loadWorkflowConfig(text: string, options: WorkflowConfigOptions): LoadedWorkflow {
	const raw = Bun.YAML.parse(text) as RawWorkflow | null;
	if (!raw || typeof raw !== "object") throw new Error("workflow must be a YAML mapping");
	assertKnownKeys(raw, WORKFLOW_KEYS, "workflow");
	if (typeof raw.name !== "string" || raw.name.trim().length === 0) throw new Error("workflow requires a name");
	if (!Array.isArray(raw.phases) || raw.phases.length === 0) throw new Error("workflow requires at least one phase");

	const maxAttempts = raw.maxAttempts === undefined ? 3 : raw.maxAttempts;
	if (!Number.isInteger(maxAttempts) || (maxAttempts as number) < 1) {
		throw new Error("maxAttempts must be a positive integer");
	}
	if (raw.acceptance !== undefined && raw.acceptance !== "review") {
		throw new Error(`unsupported acceptance mode: ${String(raw.acceptance)}`);
	}

	const rawPhases = raw.phases as RawPhase[];
	const order: string[] = [];
	const kinds = new Map<string, string>();
	for (const phase of rawPhases) {
		if (typeof phase.name !== "string" || phase.name.trim().length === 0) throw new Error("every phase needs a name");
		assertKnownKeys(phase, PHASE_KEYS, `phase "${phase.name}"`);
		if (kinds.has(phase.name)) throw new Error(`duplicate phase name: ${phase.name}`);
		const kind = phase.kind === undefined ? "agent" : phase.kind;
		if (kind !== "agent" && kind !== "code" && kind !== "fusion") {
			throw new Error(`phase "${phase.name}": kind "${String(kind)}" is not supported by this runner`);
		}
		order.push(phase.name);
		kinds.set(phase.name, kind);
	}

	// Declaration order is the implicit edge; `dependsOn: []` declares independence.
	const deps = new Map<string, string[]>();
	rawPhases.forEach((phase, index) => {
		const name = phase.name as string;
		if (phase.dependsOn === undefined) {
			deps.set(name, index === 0 ? [] : [order[index - 1] as string]);
			return;
		}
		deps.set(name, stringList(phase.dependsOn, `phase "${name}" dependsOn`));
	});

	// Paths a build legitimately rewrites without any phase claiming them.
	const undeclaredIgnore = stringList(raw.undeclaredIgnore, "undeclaredIgnore");
	const phases: GraphPhase[] = rawPhases.map(rawPhase => {
		const name = rawPhase.name as string;
		const kind = kinds.get(name);
		const gates = stringList(rawPhase.gates, `phase "${name}" gates`);
		for (const gate of gates) {
			if (!SUPPORTED_GATES.has(gate)) throw new Error(`phase "${name}": unsupported gate "${gate}"`);
		}
		const writesGlobs = rawPhase.writes === undefined ? null : stringList(rawPhase.writes, `phase "${name}" writes`);
		const inputs = stringList(rawPhase.inputs, `phase "${name}" inputs`);
		const dependsOn = deps.get(name) ?? [];
		// The prototype requires an input to be a transitive dependency, not a
		// direct one: `docs` may consume `plan` through `build`.
		const reachable = new Set<string>();
		const stack = [...dependsOn];
		while (stack.length > 0) {
			const current = stack.pop();
			if (current === undefined || reachable.has(current)) continue;
			reachable.add(current);
			stack.push(...(deps.get(current) ?? []));
		}
		for (const input of inputs) {
			if (!reachable.has(input)) {
				throw new Error(`phase "${name}" consumes "${input}" without depending on it`);
			}
		}

		if (rawPhase.onFail !== undefined && kind !== "code") {
			throw new Error(`phase "${name}" is a ${kind} phase; onFail only applies to code phases`);
		}
		if (rawPhase.onReject !== undefined && kind === "code") {
			throw new Error(`phase "${name}" is a code phase; onReject applies to phases with a review verdict`);
		}
		if (rawPhase.timeoutMs !== undefined && kind !== "code") {
			throw new Error(`phase "${name}" is a ${kind} phase; timeoutMs bounds a code phase's command`);
		}
		let onReject: GraphPhase["onReject"];
		if (rawPhase.onReject !== undefined) {
			const route = rawPhase.onReject as { to?: unknown; maxRevisions?: unknown };
			if (typeof route.to !== "string") throw new Error(`phase "${name}" onReject needs a target`);
			const budget = route.maxRevisions === undefined ? 1 : route.maxRevisions;
			if (!Number.isInteger(budget) || (budget as number) < 1) {
				throw new Error(`phase "${name}" onReject needs a positive maxRevisions`);
			}
			onReject = { to: route.to, maxRevisions: budget as number };
		} else if (rawPhase.onFail === "correct") {
			// A red command returns to the nearest dependency that can fix it.
			const target = nearestWriterAncestor(kinds, deps, name);
			if (!target) throw new Error(`phase "${name}" declares onFail: correct with no writer dependency to correct`);
			onReject = { to: target, maxRevisions: maxAttempts as number };
		} else if (rawPhase.onFail !== undefined && rawPhase.onFail !== "retry") {
			throw new Error(`phase "${name}": unsupported onFail "${String(rawPhase.onFail)}"`);
		}

		const artifactChecks = {
			exist: gates.includes("artifacts_exist"),
			nonEmpty: gates.includes("files_non_empty"),
			jsonParses: gates.includes("json_parses"),
		};
		const wantsReview = gates.includes("verdict_consistent");
		// `diff_matches_claims` is a gate a phase opts into, not a house rule:
		// running it unasked rejects honest builders in workflows that never
		// declared it.
		const matchClaims = gates.includes("diff_matches_claims");
		if (wantsReview && !options.review) {
			throw new Error(
				`phase "${name}" declares the verdict_consistent gate but no review implementation was supplied; ` +
					"a declared gate that cannot run is a silent acceptance.",
			);
		}

		if (kind === "code") {
			if (typeof rawPhase.command !== "string" || rawPhase.command.trim().length === 0) {
				throw new Error(`phase "${name}" is a code phase and needs a command`);
			}
			if (artifactChecks.exist || artifactChecks.nonEmpty) {
				throw new Error(
					`phase "${name}" is a code phase and cannot satisfy artifact gates: it declares no artifacts. ` +
						"Use diff_matches_claims, or move the gate to the agent phase that writes the files.",
				);
			}
			// The prototype's default is ten minutes. Falling back to the gate
			// runner's own 60s would SIGTERM an honest test suite and charge
			// the phantom failure to a builder whose code was never red.
			const timeoutMs = rawPhase.timeoutMs === undefined ? 600_000 : rawPhase.timeoutMs;
			if (!Number.isInteger(timeoutMs) || (timeoutMs as number) < 1) {
				throw new Error(`phase "${name}" timeoutMs must be a positive integer`);
			}
			const command = rawPhase.command;
			return {
				name,
				dependsOn,
				inputs,
				scope: [],
				assertions: [],
				gateCommand: ["sh", "-c", command],
				gateTimeoutMs: timeoutMs as number,
				// `onFail: correct` means the failure belongs to the corrector, not
				// to another run of the same command: re-running an unchanged
				// command spends the budget without changing the input.
				maxAttempts: rawPhase.onFail === "correct" ? 1 : (maxAttempts as number),
				onReject,
				produce: async () => ({ changedFiles: [], label: name, exitCode: 0 }),
			};
		}

		if (kind === "fusion") {
			const panel = Array.isArray(rawPhase.panel) ? (rawPhase.panel as RawSeat[]) : [];
			if (panel.length < 2) throw new Error(`phase "${name}" is a fusion phase and needs at least two panel seats`);
			const fuser = rawPhase.fuser as RawSeat | undefined;
			if (!fuser || typeof fuser.owner !== "string") {
				throw new Error(`phase "${name}" is a fusion phase and needs a fuser`);
			}
			const seats = panel.map((seat, index) => ({
				seat: `${name}#${index + 1}`,
				...readSeat(seat, `phase "${name}" panel seat ${index + 1}`),
			}));
			const fuserSeat = readSeat(fuser, `phase "${name}" fuser`);
			const sharedPrompt = [typeof rawPhase.prompt === "string" ? rawPhase.prompt : "", options.request ?? ""]
				.filter(part => part.trim().length > 0)
				.join("\n\n");
			// The panel is not what got rejected: a retry re-runs the fuser
			// only, and re-polling N models is the most expensive way to
			// change nothing.
			let cachedOpinions: PanelOpinion[] | undefined;
			// Resolved once per dispatch, not per attempt: see AgentRunContext.
			let entry: Promise<BaselineState> | undefined;
			return {
				name,
				dependsOn,
				inputs,
				scope: writesGlobs ?? ["**"],
				assertions: [],
				artifactChecks,
				matchClaims,
				undeclaredIgnore,
				requireArtifacts: artifactChecks.exist || artifactChecks.nonEmpty,
				writes: true,
				maxAttempts: maxAttempts as number,
				onReject,
				produce: async ({ attempt, correction, inputs: selected, workspace }) => {
					// A fresh dispatch — the first attempt after a revision —
					// must re-poll: opinions about a tree that no longer exists
					// are stale evidence wearing a current label.
					if (attempt === 1) {
						cachedOpinions = undefined;
						entry = undefined;
					}
					const entryState = (): Promise<BaselineState> => (entry ??= captureBaselineState(workspace));
					if (!cachedOpinions) {
						// Read-only seats answering the same question, so running
						// them at once is safe; order follows the declaration.
						const answers = await Promise.all(
							seats.map(seat =>
								options.runAgent({
									phase: name,
									owner: seat.owner,
									model: seat.model,
									thinking: seat.thinking,
									prompt: sharedPrompt,
									attempt,
									correction: undefined,
									inputs: selected,
									workspace,
									entryState,
									readOnly: true,
								}),
							),
						);
						cachedOpinions = answers.map((answer, index) => ({
							seat: seats[index]?.seat ?? `${name}#${index + 1}`,
							owner: seats[index]?.owner ?? "task",
							text: answer.output ?? "",
							failed: (answer.exitCode ?? 0) !== 0,
						}));
					}
					return options.runAgent({
						phase: name,
						owner: fuserSeat.owner,
						model: fuserSeat.model,
						thinking: fuserSeat.thinking,
						prompt: sharedPrompt,
						attempt,
						correction,
						inputs: selected,
						workspace,
						entryState,
						readOnly: false,
						opinions: cachedOpinions,
					});
				},
				review: wantsReview && options.review ? async candidate => options.review!(name, candidate) : undefined,
			};
		}

		const readOnly = writesGlobs !== null && writesGlobs.length === 0;
		// Resolved once per dispatch, not per attempt: see AgentRunContext.
		let agentEntry: Promise<BaselineState> | undefined;
		return {
			name,
			dependsOn,
			inputs,
			scope: writesGlobs ?? ["**"],
			assertions: [],
			artifactChecks,
			matchClaims,
			undeclaredIgnore,
			requireArtifacts: artifactChecks.exist || artifactChecks.nonEmpty,
			writes: !readOnly,
			maxAttempts: maxAttempts as number,
			onReject,
			produce: async ({ attempt, correction, inputs: selected, workspace }) => {
				if (attempt === 1) agentEntry = undefined;
				const entryState = (): Promise<BaselineState> => (agentEntry ??= captureBaselineState(workspace));
				return options.runAgent({
					phase: name,
					owner: typeof rawPhase.owner === "string" ? rawPhase.owner : "task",
					model: typeof rawPhase.model === "string" ? rawPhase.model : undefined,
					thinking: typeof rawPhase.thinking === "string" ? rawPhase.thinking : undefined,
					prompt: [typeof rawPhase.prompt === "string" ? rawPhase.prompt : "", options.request ?? ""]
						.filter(part => part.trim().length > 0)
						.join("\n\n"),
					attempt,
					correction,
					inputs: selected,
					workspace,
					entryState,
					readOnly,
				});
			},
			review: wantsReview && options.review ? async candidate => options.review!(name, candidate) : undefined,
		};
	});

	return {
		name: raw.name,
		description: typeof raw.description === "string" ? raw.description : undefined,
		isolation: raw.isolation === true,
		acceptance: raw.acceptance === "review" ? "review" : "phases",
		protectedGlobs: [...ALWAYS_PROTECTED, ...stringList(raw.protected, "protected")],
		phases,
	};
}
