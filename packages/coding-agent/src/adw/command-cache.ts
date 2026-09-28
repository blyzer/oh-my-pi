/**
 * Content-addressed memo for code-phase commands.
 *
 * A workflow re-runs the same verification constantly: a correction edits one
 * file and the loop re-runs every gate, a reviewer rewinds to a phase that
 * already passed, a fan-in re-validates producers that never changed. When the
 * inputs are byte-identical the command cannot produce a different verdict, so
 * the second run is pure latency -- and test suites are the most expensive
 * thing an ADW does.
 *
 * The key is the command plus the exact bytes it can read. Anything that is
 * not provably identical is a miss, and every failure to prove identity is a
 * miss too: a cache that guesses is worse than no cache, because a stale
 * "passed" is indistinguishable from a real one in the trace.
 */

import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import * as path from "node:path";

import { ptree } from "@oh-my-pi/pi-utils";

/** What a command produced, keyed by the state it observed. */
export type CachedRun = {
	exitCode: number;
	summary: string;
	/** Wall time of the original execution, for reporting what a hit saved. */
	elapsedMs: number;
};

/** Why a lookup did not produce a hit, for the trace. */
export type CacheMiss = "disabled" | "dirty-untracked" | "not-a-repo" | "no-entry" | "fingerprint-failed";

export type CacheLookup = { hit: true; run: CachedRun } | { hit: false; reason: CacheMiss; key: string | null };

/**
 * Fingerprint of everything a command in `cwd` could read from the repository.
 *
 * `git status --porcelain` output is folded in alongside HEAD's tree, so an
 * uncommitted edit changes the key -- the common case during a workflow, where
 * nothing is committed until integration. Untracked files are the one thing
 * this cannot see the contents of cheaply, so their presence refuses the cache
 * outright rather than keying on a name list that says nothing about bytes.
 */
async function fingerprintWorktree(
	cwd: string,
	signal?: AbortSignal,
): Promise<{ digest: string } | { refuse: CacheMiss }> {
	try {
		const head = await ptree.exec(["git", "rev-parse", "HEAD"], { cwd, timeout: 10_000, allowNonZero: true, signal });
		if (head.exitCode !== 0) return { refuse: "not-a-repo" };

		// `-z` so a filename containing a newline cannot forge a status line.
		const status = await ptree.exec(["git", "status", "--porcelain=v1", "-z", "--untracked-files=normal"], {
			cwd,
			timeout: 30_000,
			allowNonZero: true,
			signal,
		});
		if (status.exitCode !== 0) return { refuse: "fingerprint-failed" };

		const entries = status.stdout.split("\0").filter(entry => entry.length > 0);
		if (entries.some(entry => entry.startsWith("??"))) return { refuse: "dirty-untracked" };

		// Tracked modifications are real content: hash the diff itself rather
		// than the status flags, which only say *that* a file changed.
		const diff = await ptree.exec(["git", "diff", "HEAD", "--binary"], {
			cwd,
			timeout: 60_000,
			allowNonZero: true,
			signal,
		});
		if (diff.exitCode !== 0) return { refuse: "fingerprint-failed" };

		const hash = createHash("sha256");
		hash.update(head.stdout.trim());
		hash.update("\0");
		hash.update(diff.stdout);
		return { digest: hash.digest("hex") };
	} catch {
		return { refuse: "fingerprint-failed" };
	}
}

/** The cache key: what ran, where, and against which bytes. */
function cacheKey(command: string, env: Record<string, string> | undefined, worktree: string): string {
	const hash = createHash("sha256");
	hash.update(command);
	hash.update("\0");
	// Only the caller-supplied delta, sorted: the ambient environment is not
	// part of what the workflow declared and varies between machines.
	for (const name of Object.keys(env ?? {}).sort()) {
		hash.update(name);
		hash.update("=");
		hash.update(env?.[name] ?? "");
		hash.update("\0");
	}
	hash.update(worktree);
	return hash.digest("hex");
}

/**
 * A run-scoped memo of command verdicts.
 *
 * Scoped to one workflow run, not the machine: a cross-run cache would have to
 * reason about toolchain upgrades, changed environment and clock-dependent
 * tests, none of which the fingerprint can see. Within a run those are fixed.
 */
export class CommandCache {
	readonly #entries = new Map<string, CachedRun>();
	readonly #enabled: boolean;
	#hits = 0;
	#savedMs = 0;

	constructor(enabled: boolean) {
		this.#enabled = enabled;
	}

	/** Hits so far and the wall time they avoided. */
	get stats(): { hits: number; savedMs: number } {
		return { hits: this.#hits, savedMs: this.#savedMs };
	}

	/**
	 * Look up a command against the current worktree.
	 *
	 * Returns the key on a miss so the caller can store the result under it
	 * without fingerprinting twice -- and so a command that mutated the tree
	 * is stored against the state it *observed*, not the one it produced.
	 */
	async lookup(
		command: string,
		cwd: string,
		env: Record<string, string> | undefined,
		signal?: AbortSignal,
	): Promise<CacheLookup> {
		if (!this.#enabled) return { hit: false, reason: "disabled", key: null };
		const fingerprint = await fingerprintWorktree(cwd, signal);
		if ("refuse" in fingerprint) return { hit: false, reason: fingerprint.refuse, key: null };

		const key = cacheKey(command, env, fingerprint.digest);
		const run = this.#entries.get(key);
		if (!run) return { hit: false, reason: "no-entry", key };

		this.#hits += 1;
		this.#savedMs += run.elapsedMs;
		return { hit: true, run };
	}

	/** Record a verdict under the key its lookup returned. */
	store(key: string | null, run: CachedRun): void {
		if (key === null) return;
		this.#entries.set(key, run);
	}
}

/**
 * Whether a phase may be memoized.
 *
 * Opt-in per phase. A command is only cacheable if re-running it against
 * identical inputs is guaranteed to produce the identical verdict, and only
 * the workflow author knows that: `cargo test` qualifies, `date`, `curl` and
 * anything that publishes do not. Defaulting this on would eventually cache a
 * deploy.
 */
export function phaseIsCacheable(phase: { cache?: boolean; expect?: string }): boolean {
	// `expect: fail` phases are never memoized. Their whole purpose is proving
	// a test genuinely runs and genuinely fails; serving that from a memo
	// would satisfy the criterion without executing anything, which is the
	// exact failure the inverted criterion exists to catch.
	if (phase.expect === "fail") return false;
	return phase.cache === true;
}

/** Path a run writes its cache report to, for the trace. */
export function cacheReportPath(runDir: string): string {
	return path.join(runDir, "command-cache.json");
}

/** Persist hit/saving counts beside the run's other evidence. */
export async function writeCacheReport(runDir: string, cache: CommandCache): Promise<void> {
	const { hits, savedMs } = cache.stats;
	if (hits === 0) return;
	await fs.writeFile(cacheReportPath(runDir), `${JSON.stringify({ hits, savedMs }, null, 2)}\n`, "utf8");
}
