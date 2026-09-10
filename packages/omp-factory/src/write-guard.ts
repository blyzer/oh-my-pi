/**
 * Attempt-boundary write guard: detect, classify, and ROLL BACK what a
 * rejected attempt wrote.
 *
 * Refusing a candidate is not the same as undoing it. On a shared workspace
 * a rejected attempt's files stay on disk, so the next attempt inherits
 * them and the operator's checkout carries work nothing accepted. OMP core
 * owns the machinery (`TaskWriteGuard`): a durable baseline that tells the
 * user's own dirt apart from this attempt's writes, file-granular restore,
 * and a preserved patch when a restore cannot be done safely.
 *
 * Injected rather than assumed, because it is not free. The scan
 * deliberately covers ignored files — an ignore rule must not hide a
 * protected path — so its cost tracks the whole tree, not the tracked set.
 * Measured on this repo: a clean 7k-file worktree costs ~8s to snapshot and
 * ~1s to settle; the same checkout carrying `target/` and `node_modules`
 * costs ~220s and 32GB of stored objects. A caller that cannot pay that
 * isolates instead, and one that does neither has to say so.
 */
import path from "node:path";
import { TaskWriteGuard } from "@oh-my-pi/pi-natives";

export interface GuardReport {
	/** Paths changed outside the declared scope, or protected outright. */
	unauthorized: string[];
	/** Restored byte-for-byte to their state at the attempt boundary. */
	rolledBack: string[];
	/** Left as written: restoring was unsafe. The caller must fail closed. */
	unrecoverable: string[];
	/** Where the unauthorized diff was preserved; set only when unrecoverable. */
	patchPath?: string;
}

/** One phase's guard: `begin` at each attempt, `settle` once it produces. */
export interface PhaseGuard {
	begin: () => void;
	settle: (allowed: string[], protectedGlobs: string[]) => GuardReport;
}

export interface WriteGuardProvider {
	open: (phase: string, root: string, stateDir: string) => PhaseGuard;
}

/** Backed by core's `TaskWriteGuard`. Needs a git repo at `root`. */
export function nativeWriteGuard(): WriteGuardProvider {
	return {
		open: (phase, root, stateDir) => {
			const guard = TaskWriteGuard.create({
				root,
				baselineFile: path.join(stateDir, `guard-${phase}.json`),
			});
			const patchDir = path.join(stateDir, "guard-patches");
			return {
				begin: () => guard.begin(),
				settle: (allowed, protectedGlobs) => {
					const report = guard.settle({ allowed, protectedGlobs, patchDir });
					return {
						unauthorized: report.unauthorized.map(change => change.path),
						rolledBack: [...report.rolledBack],
						unrecoverable: [...report.unrecoverable],
						patchPath: report.patchPath ?? undefined,
					};
				},
			};
		},
	};
}
