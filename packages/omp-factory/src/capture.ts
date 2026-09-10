/**
 * Change capture: the driver scope-checks the ACTUAL change-set, never the
 * producer's declaration. Baseline/delta logic is owned by OMP core
 * (`captureBaseline`/`captureDeltaPatch`); this module only composes it.
 */
import {
	captureBaseline,
	captureDeltaPatch,
	getRepoRoot,
	patchTouchedFiles,
	type WorktreeBaseline,
} from "@oh-my-pi/pi-coding-agent";

export interface BaselineState {
	repoRoot: string;
	baseline: WorktreeBaseline;
}

export async function captureBaselineState(cwd: string): Promise<BaselineState> {
	const repoRoot = await getRepoRoot(cwd);
	return { repoRoot, baseline: await captureBaseline(repoRoot) };
}

/** Repo-relative touched files since the baseline, including nested repos. */
export async function captureTouchedSince(state: BaselineState): Promise<string[]> {
	const delta = await captureDeltaPatch(state.repoRoot, state.baseline);
	const touched = new Set<string>(patchTouchedFiles(delta.rootPatch));
	for (const nested of delta.nestedPatches) {
		for (const file of patchTouchedFiles(nested.patch)) {
			touched.add(`${nested.relativePath}/${file}`);
		}
	}
	return [...touched].sort();
}
