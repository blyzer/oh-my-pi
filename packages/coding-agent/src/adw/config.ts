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
import { type AdwWorkflowConfig, adwWorkflowSchema, type DiscoveredWorkflow } from "./types";

/** Only the native config root holds workflows; `.claude`/`.codex` are not ours. */
const ADW_CONFIG_SOURCE = ".omp";
const ADW_SUBPATH = "adw";

export class AdwConfigError extends Error {}

/**
 * The phase a failing `code` phase sends its failure back to.
 *
 * With `dependsOn` the answer is explicit: walk this phase's own dependencies,
 * nearest first, for one that has an agent to correct. That is strictly better
 * than guessing from position — a phase declared earlier but unrelated has no
 * business receiving another's failure.
 *
 * Without dependencies, fall back to the nearest preceding declared phase,
 * which is what ordering meant before a graph existed.
 *
 * Resolved here rather than in the engine so no phase-selection policy lives
 * in Rust — it is handed a name and obeys it. A `code` phase is never a target:
 * correcting it would mean re-running a command that already decided.
 */
export function rewindTarget(workflow: AdwWorkflowConfig, phaseName: string): string | undefined {
	const byName = new Map(workflow.phases.map(phase => [phase.name, phase]));
	const start = byName.get(phaseName);
	if (!start) return undefined;
	const correctable = (name: string) => {
		const kind = byName.get(name)?.kind;
		return kind === "agent" || kind === "fusion";
	};

	if (start.dependsOn?.length) {
		// Breadth-first: the closest dependency that can act, not the deepest.
		const queue = [...start.dependsOn];
		const seen = new Set<string>(queue);
		while (queue.length > 0) {
			const name = queue.shift();
			if (!name) break;
			if (correctable(name)) return name;
			for (const next of byName.get(name)?.dependsOn ?? []) {
				if (!seen.has(next)) {
					seen.add(next);
					queue.push(next);
				}
			}
		}
		return undefined;
	}

	const index = workflow.phases.findIndex(phase => phase.name === phaseName);
	for (let i = index - 1; i >= 0; i--) {
		const candidate = workflow.phases[i];
		if (candidate && correctable(candidate.name)) return candidate.name;
	}
	return undefined;
}

/**
 * Rejects a schema-valid file that cannot run: an `agent` phase with no owner,
 * a `code` phase with no command, an unknown gate, or duplicate phase names
 * (the engine keys gates and trace records by name).
 */
function validate(workflow: AdwWorkflowConfig, source: string): void {
	const fail = (message: string) => {
		throw new AdwConfigError(`${source}: ${message}`);
	};
	if (workflow.phases.length === 0) fail("workflow has no phases");
	if (workflow.maxAttempts !== undefined && (!Number.isInteger(workflow.maxAttempts) || workflow.maxAttempts < 1)) {
		// A fractional value makes the engine's clamp and the driver's step
		// budget compute different numbers from the same input.
		fail(`maxAttempts must be a whole number >= 1, got ${workflow.maxAttempts}`);
	}
	// The engine is the source of truth: a gate added or removed in Rust must
	// not need a matching edit here to be accepted or rejected.
	const knownGates = taskGateNames();

	const seen = new Set<string>();
	for (const phase of workflow.phases) {
		if (seen.has(phase.name)) fail(`duplicate phase name "${phase.name}"`);
		seen.add(phase.name);

		if (phase.kind === "agent" && !phase.owner) fail(`phase "${phase.name}" is an agent phase but names no owner`);
		if (phase.kind === "code" && !phase.command) fail(`phase "${phase.name}" is a code phase but has no command`);
		if (phase.kind === "code" && (phase.model || phase.thinking || phase.prompt)) {
			fail(`phase "${phase.name}" is a code phase; model/thinking/prompt do not apply`);
		}
		if (phase.onFail && phase.kind !== "code") {
			fail(`phase "${phase.name}" is a ${phase.kind} phase; onFail only applies to code phases`);
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
		// and an empty claim now fails these gates rather than passing
		// vacuously — so this combination is a guaranteed runtime failure.
		// Catching it here costs nothing; catching it mid-run costs a phase.
		if (phase.kind === "code") {
			for (const gate of phase.gates ?? []) {
				if (gate === "artifacts_exist" || gate === "files_non_empty") {
					fail(
						`phase "${phase.name}" is a code phase and cannot satisfy gate "${gate}": ` +
							"a code phase declares no artifacts. Use diff_matches_claims, or move the gate to the " +
							"agent phase that writes the files.",
					);
				}
			}
		}
		for (const dependency of phase.dependsOn ?? []) {
			if (dependency === phase.name) fail(`phase "${phase.name}" depends on itself`);
			if (!workflow.phases.some(other => other.name === dependency)) {
				fail(`phase "${phase.name}" depends on unknown phase "${dependency}"`);
			}
		}
	}
	assertAcyclic(workflow, fail);
}

/**
 * A cycle is reported here, with the file it came from, because the engine
 * cannot: it leaves an unorderable graph in declaration order rather than
 * inventing one, which would run the workflow in a sequence the author never
 * wrote. Names the phases still standing, so the operator sees the loop.
 */
function assertAcyclic(workflow: AdwWorkflowConfig, fail: (message: string) => void): void {
	const known = new Set(workflow.phases.map(phase => phase.name));
	const pending = new Map(
		workflow.phases.map(phase => [phase.name, new Set((phase.dependsOn ?? []).filter(name => known.has(name)))]),
	);
	for (;;) {
		const ready = [...pending].filter(([, deps]) => deps.size === 0).map(([name]) => name);
		if (ready.length === 0) break;
		for (const name of ready) pending.delete(name);
		for (const deps of pending.values()) for (const name of ready) deps.delete(name);
	}
	if (pending.size > 0) {
		fail(`dependency cycle among phases: ${[...pending.keys()].sort().join(", ")}`);
	}
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
