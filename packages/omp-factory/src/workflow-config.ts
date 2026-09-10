/**
 * Load the frozen ADW workflow format into graph phases.
 *
 * The declarative contract is the prototype's, so an existing `.omp/adw/*.yml`
 * runs here without being rewritten. Anything this runner cannot honour is
 * refused at load time rather than silently dropped — a workflow that parses
 * must mean what it says.
 */
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
	phases?: unknown;
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

/** Nearest ancestor that owns an agent — the only phase that can act on a correction. */
function nearestAgentAncestor(
	order: string[],
	kinds: Map<string, string>,
	deps: Map<string, string[]>,
	from: string,
): string | null {
	const queue = [...(deps.get(from) ?? [])];
	const seen = new Set<string>();
	while (queue.length > 0) {
		const current = queue.shift();
		if (current === undefined || seen.has(current)) continue;
		seen.add(current);
		if (kinds.get(current) === "agent") return current;
		queue.push(...(deps.get(current) ?? []));
	}
	return null;
}

export function loadWorkflowConfig(text: string, options: WorkflowConfigOptions): LoadedWorkflow {
	const raw = Bun.YAML.parse(text) as RawWorkflow | null;
	if (!raw || typeof raw !== "object") throw new Error("workflow must be a YAML mapping");
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
			const target = nearestAgentAncestor(order, kinds, deps, name);
			if (!target) throw new Error(`phase "${name}" declares onFail: correct with no agent dependency to correct`);
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
			const command = rawPhase.command;
			return {
				name,
				dependsOn,
				inputs,
				scope: [],
				assertions: [],
				gateCommand: ["sh", "-c", command],
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
			return {
				name,
				dependsOn,
				inputs,
				scope: writesGlobs ?? ["**"],
				assertions: [],
				artifactChecks,
				requireArtifacts: artifactChecks.exist || artifactChecks.nonEmpty,
				writes: true,
				maxAttempts: maxAttempts as number,
				onReject,
				produce: async ({ attempt, correction, inputs: selected, workspace }) => {
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
						readOnly: false,
						opinions: cachedOpinions,
					});
				},
				review: wantsReview && options.review ? async candidate => options.review!(name, candidate) : undefined,
			};
		}

		const readOnly = writesGlobs !== null && writesGlobs.length === 0;
		return {
			name,
			dependsOn,
			inputs,
			scope: writesGlobs ?? ["**"],
			assertions: [],
			artifactChecks,
			requireArtifacts: artifactChecks.exist || artifactChecks.nonEmpty,
			writes: !readOnly,
			maxAttempts: maxAttempts as number,
			onReject,
			produce: async ({ attempt, correction, inputs: selected, workspace }) =>
				options.runAgent({
					phase: name,
					owner: typeof rawPhase.owner === "string" ? rawPhase.owner : "task",
					prompt: [typeof rawPhase.prompt === "string" ? rawPhase.prompt : "", options.request ?? ""]
						.filter(part => part.trim().length > 0)
						.join("\n\n"),
					attempt,
					correction,
					inputs: selected,
					workspace,
					readOnly,
				}),
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
