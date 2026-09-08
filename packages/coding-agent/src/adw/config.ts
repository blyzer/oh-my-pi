/**
 * Workflow discovery: `.omp/adw/<name>.yml`.
 *
 * Mirrors agent discovery (`.omp/agents/*.md`) so a repo carries its factory
 * next to its roster: project files win over user-level ones, and a malformed
 * file is reported by name instead of silently skipped — a workflow the user
 * asked for by name must never resolve to something else.
 */

import * as path from "node:path";
import { type } from "@oh-my-pi/omptype";
import { YAML } from "bun";
import { getConfigDirs } from "../config";
import { taskGateNames } from "@oh-my-pi/pi-natives";
import { compilePhaseChecks, VERDICT_GATE } from "./schema";
import { type AdwPhaseConfig, type AdwWorkflowConfig, adwWorkflowSchema, type DiscoveredWorkflow } from "./types";

/** Only the native config root holds workflows; `.claude`/`.codex` are not ours. */
const ADW_CONFIG_SOURCE = ".omp";
const ADW_SUBPATH = "adw";

export class AdwConfigError extends Error {}

/**
 * The phase a failing `code` phase sends its failure back to.
 *
 * Walk the effective dependency graph nearest first. An omitted dependsOn
 * follows the declaration predecessor; an explicit [] stops that branch.
 * A code phase can never correct another command's result.
 */
export function rewindTarget(workflow: AdwWorkflowConfig, phaseName: string): string | undefined {
	const byName = new Map(workflow.phases.map(phase => [phase.name, phase]));
	const graph = dependencyGraph(workflow);
	for (const name of dependencies(graph, graph.get(phaseName) ?? [])) {
		const kind = byName.get(name)?.kind;
		if (kind === "agent" || kind === "fusion") return name;
	}
	return undefined;
}

/** Preserve omitted versus explicit-empty dependencies in every graph traversal. */
function dependencyGraph(workflow: AdwWorkflowConfig): Map<string, readonly string[]> {
	return new Map(
		workflow.phases.map((phase, index) => [
			phase.name,
			phase.dependsOn ?? (index > 0 ? [workflow.phases[index - 1]!.name] : []),
		]),
	);
}

/** Breadth-first: the closest dependency first, shared by correction and review routes. */
function* dependencies(graph: ReadonlyMap<string, readonly string[]>, roots: readonly string[]): Generator<string> {
	const queue = [...roots];
	const seen = new Set<string>(queue);
	for (let index = 0; index < queue.length; index++) {
		const name = queue[index]!;
		yield name;
		for (const next of graph.get(name) ?? []) {
			if (!seen.has(next)) {
				seen.add(next);
				queue.push(next);
			}
		}
	}
}

/** Whether `target` is reachable through `roots`' transitive `dependsOn` closure. */
function dependsTransitively(
	graph: ReadonlyMap<string, readonly string[]>,
	roots: readonly string[],
	target: string,
): boolean {
	for (const name of dependencies(graph, roots)) if (name === target) return true;
	return false;
}

/**
 * `writes`/`protected` compile to native globsets when the write guard runs,
 * where a malformed pattern fails naming itself. No TS-side compiler shares
 * those semantics, so load time rejects only entries that are wrong under any
 * semantics: blank globs and duplicates.
 */
function checkGlobList(globs: readonly string[], label: string, fail: (message: string) => never): void {
	const seen = new Set<string>();
	for (const glob of globs) {
		if (glob.trim().length === 0) fail(`${label} contains an empty glob`);
		if (seen.has(glob)) fail(`${label} lists "${glob}" more than once`);
		seen.add(glob);
	}
}

/**
 * Rejects a schema-valid file that cannot run: an `agent` phase with no owner,
 * a `code` phase with no command, an unknown gate, or duplicate phase names
 * (the engine keys gates and trace records by name).
 */
function validate(workflow: AdwWorkflowConfig, source: string): void {
	const fail: (message: string) => never = message => {
		throw new AdwConfigError(`${source}: ${message}`);
	};
	if (workflow.phases.length === 0) fail("workflow has no phases");
	if (workflow.maxAttempts !== undefined && (!Number.isInteger(workflow.maxAttempts) || workflow.maxAttempts < 1)) {
		// Attempt accounting is discrete; do not silently round the author's budget.
		fail(`maxAttempts must be a whole number >= 1, got ${workflow.maxAttempts}`);
	}
	if (
		workflow.concurrency !== undefined &&
		(!Number.isInteger(workflow.concurrency) || workflow.concurrency < 1 || workflow.concurrency > 8)
	) {
		// The dispatch width is a seat count; fractions have no meaning and an
		// unbounded width would let one workflow saturate the machine.
		fail(`concurrency must be a whole number from 1 to 8, got ${workflow.concurrency}`);
	}
	const concurrent = (workflow.concurrency ?? 1) > 1;
	if (concurrent && workflow.isolation !== true) {
		fail("concurrency greater than 1 requires isolation: true to keep refused integration out of the user checkout");
	}
	if (workflow.protected) checkGlobList(workflow.protected, "protected", fail);
	// Native gates are discovered from the engine; review checks run in the caller.
	const knownGates = [...taskGateNames(), VERDICT_GATE];

	const seen = new Set<string>();
	for (const phase of workflow.phases) {
		if (seen.has(phase.name)) fail(`duplicate phase name "${phase.name}"`);
		seen.add(phase.name);

		if (phase.kind === "agent" && !phase.owner) fail(`phase "${phase.name}" is an agent phase but names no owner`);
		if (phase.kind === "code" && !phase.command) fail(`phase "${phase.name}" is a code phase but has no command`);
		if (phase.kind === "code" && (phase.model || phase.thinking || phase.prompt)) {
			fail(`phase "${phase.name}" is a code phase; model/thinking/prompt do not apply`);
		}
		if (concurrent && phase.kind !== "code" && phase.writes === undefined) {
			fail(`phase "${phase.name}" requires explicit writes with concurrency greater than 1; use [] to deny writes`);
		}
		if (phase.writes) {
			// A code phase's writer is a command, and the guard scopes agents;
			// accepting the key here would promise an enforcement that never runs.
			if (phase.kind === "code") {
				fail(`phase "${phase.name}" is a code phase; writes only applies to agent and fusion phases`);
			}
			checkGlobList(phase.writes, `phase "${phase.name}" writes`, fail);
		}
		if (phase.onFail && phase.kind !== "code") {
			fail(`phase "${phase.name}" is a ${phase.kind} phase; onFail only applies to code phases`);
		}
		if (phase.onReject) {
			if (phase.kind === "code") {
				fail(`phase "${phase.name}" is a code phase; onReject requires an agent or fusion review envelope`);
			}
			if (!phase.gates?.includes(VERDICT_GATE)) {
				fail(`phase "${phase.name}" sets onReject but requires the ${VERDICT_GATE} gate`);
			}
			const { maxRevisions } = phase.onReject;
			if (!Number.isInteger(maxRevisions) || maxRevisions < 1 || maxRevisions > 65535) {
				fail(
					`phase "${phase.name}" onReject.maxRevisions must be a whole number from 1 to 65535, got ${maxRevisions}`,
				);
			}
		}
		// A target that does not exist is a config error, and the engine would
		// only discover it mid-run — after a phase already failed, which is the
		// worst moment to learn the workflow was malformed.
		if (phase.onFail === "correct" && !rewindTarget(workflow, phase.name)) {
			fail(`phase "${phase.name}" sets onFail: correct but no agent or fusion phase precedes it`);
		}
		if (phase.kind === "fusion") {
			// One seat is not a panel — it is an `agent` phase with extra syntax.
			if ((phase.panel?.length ?? 0) < 2)
				fail(`phase "${phase.name}" is a fusion phase and needs at least two panel seats`);
			if (!phase.fuser?.owner) fail(`phase "${phase.name}" is a fusion phase but names no fuser`);
			if (phase.command) fail(`phase "${phase.name}" is a fusion phase; command does not apply`);
		} else if (phase.panel || phase.fuser) {
			fail(`phase "${phase.name}" is a ${phase.kind} phase; panel/fuser only apply to fusion phases`);
		}
		if (phase.timeoutMs !== undefined && (!Number.isFinite(phase.timeoutMs) || phase.timeoutMs <= 0)) {
			// ptree attaches no deadline at all for `<= 0`, so the phase would hang
			// with no timeout and — today — no cancellation path.
			fail(`phase "${phase.name}" has timeoutMs ${phase.timeoutMs}; it must be greater than zero`);
		}
		for (const gate of phase.gates ?? []) {
			if (!knownGates.includes(gate)) {
				fail(`phase "${phase.name}" requests unknown gate "${gate}" (known: ${knownGates.join(", ")})`);
			}
		}
		// A `code` phase reports an envelope with no artifacts by construction,
		// and an empty claim now fails every artifact-scoped gate rather than
		// passing vacuously — so this combination is a guaranteed runtime
		// failure. Catching it here costs nothing; mid-run it costs a phase.
		const CLAIM_GATES = ["artifacts_exist", "files_non_empty", "json_parses"];
		if (phase.kind === "code") {
			if (phase.gates?.includes(VERDICT_GATE)) {
				fail(`phase "${phase.name}" is a code phase; ${VERDICT_GATE} requires an agent or fusion review envelope`);
			}
			for (const gate of phase.gates ?? []) {
				if (CLAIM_GATES.includes(gate)) {
					fail(
						`phase "${phase.name}" is a code phase and cannot satisfy gate "${gate}": ` +
							"a code phase declares no artifacts. Use diff_matches_claims, or move the gate to the " +
							"agent phase that writes the files.",
					);
				}
			}
		}
		if (phase.schema !== undefined) {
			if (phase.kind === "code") {
				fail(`phase "${phase.name}" is a code phase; schema describes an envelope payload and does not apply`);
			}
		}
		if (phase.schema !== undefined || phase.gates?.includes(VERDICT_GATE)) {
			const compiled = compilePhaseChecks(phase);
			if (typeof compiled === "string") fail(`phase "${phase.name}" has an unusable schema: ${compiled}`);
		}
		for (const dependency of phase.dependsOn ?? []) {
			if (dependency === phase.name) fail(`phase "${phase.name}" depends on itself`);
			if (!workflow.phases.some(other => other.name === dependency)) {
				fail(`phase "${phase.name}" depends on unknown phase "${dependency}"`);
			}
		}
		const declaredInputs = new Set<string>();
		for (const input of phase.inputs ?? []) {
			// An input names a producer whose accepted output this phase reads.
			// A name that resolves to nothing — or to the phase itself — would
			// only surface at dispatch, as a selection failure mid-run.
			if (input === phase.name) fail(`phase "${phase.name}" lists itself as an input`);
			if (declaredInputs.has(input)) fail(`phase "${phase.name}" lists input "${input}" more than once`);
			declaredInputs.add(input);
			if (!workflow.phases.some(other => other.name === input)) {
				fail(`phase "${phase.name}" consumes unknown input "${input}"`);
			}
		}
	}
	assertAcyclic(workflow, fail);
	const byName = new Map(workflow.phases.map(phase => [phase.name, phase]));
	const graph = dependencyGraph(workflow);
	for (const phase of workflow.phases) {
		if (!phase.onReject) continue;
		const target = byName.get(phase.onReject.to);
		if (!target) fail(`phase "${phase.name}" onReject targets unknown phase "${phase.onReject.to}"`);
		if (target.name === phase.name) fail(`phase "${phase.name}" onReject targets itself`);
		if (target.kind === "code")
			fail(`phase "${phase.name}" onReject target "${target.name}" must be an agent or fusion phase`);
		// Causally upstream is the whole requirement: a topological "earlier"
		// is undefined between independent phases in a concurrent DAG, and a
		// transitive dependency executes first in every schedule by definition.
		if (!dependsTransitively(graph, graph.get(phase.name)!, target.name)) {
			fail(`phase "${phase.name}" onReject target "${target.name}" is not a transitive dependency`);
		}
	}
	// Inputs must be causally upstream, not merely earlier in one of the
	// possible topological orders.
	for (const phase of workflow.phases) {
		for (const input of phase.inputs ?? []) {
			if (!dependsTransitively(graph, graph.get(phase.name)!, input)) {
				fail(`phase "${phase.name}" input "${input}" is not a transitive dependency`);
			}
		}
	}
	if (concurrent && workflow.acceptance === "review") {
		const review = workflow.phases.at(-1)!;
		if (!review.gates?.includes(VERDICT_GATE)) {
			fail(`concurrent review acceptance requires final phase "${review.name}" to use ${VERDICT_GATE}`);
		}
		for (const phase of workflow.phases) {
			if (phase.name === review.name || phase.kind === "code" || phase.writes?.length === 0) continue;
			if (!dependsTransitively(graph, graph.get(review.name)!, phase.name)) {
				fail(`final review "${review.name}" must depend on concurrent writer "${phase.name}" to verify combined changes`);
			}
		}
	}
}

/**
 * A cycle is reported here, with the file it came from, because the engine
 * cannot: it leaves an unorderable graph in declaration order rather than
 * inventing one, which would run the workflow in a sequence the author never
 * wrote. Names the phases still standing, so the operator sees the loop.
 */
function assertAcyclic(workflow: AdwWorkflowConfig, fail: (message: string) => void): string[] {
	const known = new Set(workflow.phases.map(phase => phase.name));
	const pending = new Map(
		[...dependencyGraph(workflow)].map(([name, deps]) => [name, new Set(deps.filter(dep => known.has(dep)))]),
	);
	const order: string[] = [];
	for (;;) {
		const ready = [...pending].filter(([, deps]) => deps.size === 0).map(([name]) => name);
		if (ready.length === 0) break;
		for (const name of ready) pending.delete(name);
		order.push(...ready);
		for (const deps of pending.values()) for (const name of ready) deps.delete(name);
	}
	if (pending.size > 0) {
		fail(`dependency cycle among phases: ${[...pending.keys()].sort().join(", ")}`);
	}
	return order;
}

/** Parses one workflow file. Throws {@link AdwConfigError} with the path on any problem. */
export function parseWorkflow(source: string, text: string): AdwWorkflowConfig {
	let raw: unknown;
	try {
		raw = YAML.parse(text);
	} catch (err) {
		throw new AdwConfigError(`${source}: invalid YAML — ${err instanceof Error ? err.message : String(err)}`);
	}
	if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
		throw new AdwConfigError(`${source}: expected a YAML mapping`);
	}
	const result = adwWorkflowSchema(raw);
	if (result instanceof type.errors) throw new AdwConfigError(`${source}: ${result.summary}`);
	validate(result, source);
	return result;
}

/** Workflow directories, project (nearest first) then user. */
function workflowDirs(cwd: string): { path: string; level: "user" | "project" }[] {
	return getConfigDirs(ADW_SUBPATH, { cwd, existingOnly: true })
		.filter(entry => entry.source === ADW_CONFIG_SOURCE)
		.map(entry => ({ path: entry.path, level: entry.level }));
}

/** Everything a discovery pass found, including the files it could not read. */
export interface WorkflowDiscovery {
	workflows: Map<string, DiscoveredWorkflow>;
	/** One message per unreadable file. Never silently dropped: a broken file the
	 *  operator is about to ask for by name must explain itself, not read as absent. */
	problems: string[];
}

/**
 * Every readable workflow, keyed by name. Project files shadow user files;
 * within one directory the file name is irrelevant — `name:` decides.
 */
export async function discoverWorkflows(cwd: string): Promise<WorkflowDiscovery> {
	const workflows = new Map<string, DiscoveredWorkflow>();
	const problems: string[] = [];

	for (const dir of workflowDirs(cwd)) {
		const files = [...new Bun.Glob("*.{yml,yaml}").scanSync({ cwd: dir.path, absolute: true })].sort();
		for (const file of files) {
			try {
				const workflow = parseWorkflow(path.basename(file), await Bun.file(file).text());
				if (!workflows.has(workflow.name)) workflows.set(workflow.name, { workflow, path: file, level: dir.level });
			} catch (err) {
				problems.push(err instanceof Error ? err.message : String(err));
			}
		}
	}
	return { workflows, problems };
}

/** Resolves one workflow by name, or explains what was available and what was broken. */
export async function loadWorkflow(cwd: string, name: string): Promise<DiscoveredWorkflow> {
	const { workflows, problems } = await discoverWorkflows(cwd);
	const hit = workflows.get(name);
	if (hit) return hit;
	const available = [...workflows.keys()].sort().join(", ") || "none";
	// The parse error is the one thing the operator needs when the workflow they
	// named is the file that failed to load.
	const broken = problems.length > 0 ? `\nFiles that failed to load:\n  ${problems.join("\n  ")}` : "";
	throw new AdwConfigError(
		`Unknown workflow "${name}". Available: ${available}. Workflows live in .omp/adw/*.yml${broken}`,
	);
}
