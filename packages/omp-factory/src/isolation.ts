/**
 * Workspace isolation for parallel writers.
 *
 * `copyIsolation` materialises each writer's own tree with a plain recursive
 * copy: portable, dependency-free, and enough to make concurrent writers
 * safe, since the runner lands accepted diffs serially onto the shared root.
 * OMP core owns faster copy-on-write backends (`ensureIsolation`), which this
 * package will adopt once they are reachable from an external plugin.
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type { IsolationProvider } from "./graph";

export interface CopyIsolationOptions {
	/** Directory holding the sandboxes; defaults to the system temp dir. */
	parentDir?: string;
	/** Names never copied into a sandbox. */
	exclude?: string[];
}

/**
 * `.git` is deliberately NOT excluded: change capture and any git-aware tool
 * inside the sandbox need the repository. Only bulk that no verifier reads is
 * skipped by default.
 */
const DEFAULT_EXCLUDE = ["node_modules"];

/** Sandbox id characters that are safe in a directory name. */
function safeSegment(id: string): string {
	return id.replace(/[^A-Za-z0-9_-]+/g, "-").slice(0, 64);
}

export function copyIsolation(options: CopyIsolationOptions = {}): IsolationProvider {
	const exclude = new Set(options.exclude ?? DEFAULT_EXCLUDE);
	return {
		async start(id: string, baseCwd: string) {
			const parent = options.parentDir ?? os.tmpdir();
			const dir = await fs.mkdtemp(path.join(parent, `omp-factory-iso-${safeSegment(id)}-`));
			await fs.cp(baseCwd, dir, {
				recursive: true,
				filter: source => {
					const relative = path.relative(baseCwd, source);
					if (relative === "") return true;
					return !relative.split(path.sep).some(segment => exclude.has(segment));
				},
			});
			return {
				dir,
				stop: async () => {
					await fs.rm(dir, { recursive: true, force: true });
				},
			};
		},
	};
}
