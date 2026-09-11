import * as fs from "node:fs/promises";
import * as path from "node:path";
import { TaskTraceReader, TaskWriteGuard, taskTraceLayout } from "@oh-my-pi/pi-natives";
import * as vcs from "@oh-my-pi/pi-natives/vcs";
import { isEnoent } from "@oh-my-pi/pi-utils";
import type { DeltaPatchResult } from "../task/worktree";

/** Run-state records never become visible half-written. */
export async function writeRunState(file: string, value: unknown): Promise<void> {
	await Bun.write(`${file}.tmp`, JSON.stringify(value));
	await fs.rename(`${file}.tmp`, file);
}

export interface IntegrationRecord {
	phase: string;
	fromSeq: number;
	delta: DeltaPatchResult;
	status: "prepared" | "applying" | "integrated" | "rejected";
}

export async function preserveDelta(dir: string, delta: DeltaPatchResult): Promise<void> {
	if (delta.rootPatch.trim()) await Bun.write(path.join(dir, "changes.patch"), delta.rootPatch);
	for (const [index, nested] of delta.nestedPatches.entries()) {
		await Bun.write(path.join(dir, `nested-${index}.patch`), nested.patch);
	}
	await writeRunState(path.join(dir, "delta.json"), delta);
}

function acceptedSubmission(traceDir: string, record: IntegrationRecord): boolean {
	const reader = new TaskTraceReader(traceDir);
	const raw = reader.readRaw(record.fromSeq);
	const strings = reader.strings();
	const layout = taskTraceLayout();
	let accepted = false;
	for (let at = 0; at + layout.recordLen <= raw.length; at += layout.recordLen) {
		if (strings[raw.readUInt32LE(at + layout.offPhase) - 1] !== record.phase) continue;
		const kind = layout.kindNames[raw[at + layout.offKind] as number];
		if (kind === "phase_finished") accepted = ((raw[at + layout.offFlags] as number) & layout.flagOk) !== 0;
		if (kind === "phase_invalidated") accepted = false;
	}
	return accepted;
}

/**
 * Only the driver mutates the private integration root. A persisted guard
 * boundary lets recovery undo an interrupted multi-repository apply before
 * replaying the accepted patch. No staging, commits, or nested-repo stashes.
 *
 * `verifyDelivered` closes the gap between a verified CANDIDATE and a
 * verified DELIVERY. The patch was checked in the workspace it was produced
 * in; it lands somewhere else, and two change-sets can each be valid alone
 * and broken together. When the delivered tree fails, the same guard
 * boundary that would undo an interrupted apply undoes this one — so an
 * unverifiable delivery is never left behind.
 */
export async function integrateAccepted(
	root: string,
	runDir: string,
	record: IntegrationRecord,
	verifyDelivered?: (root: string) => Promise<{ ok: true } | { ok: false; evidence: string }>,
): Promise<void> {
	const journal = path.join(runDir, "integration.json");
	const guard = TaskWriteGuard.create({
		root,
		baselineFile: path.join(runDir, `integration-${record.fromSeq}.baseline.json`),
	});
	// I10: a record that already landed must not land again. `git apply` is
	// not idempotent — re-applying an accepted patch appends its additions a
	// second time, silently doubling them in the delivered tree. Measured:
	// a one-line addition became two identical lines.
	if (record.status === "integrated") {
		throw new Error(`integration for phase "${record.phase}" already landed; refusing to apply it twice`);
	}
	if (record.status === "rejected") {
		throw new Error(`integration for phase "${record.phase}" was rejected; it cannot be replayed`);
	}
	if (record.status === "applying") {
		const restored = guard.settle({ allowed: [], protectedGlobs: [], patchDir: runDir });
		if (restored.unrecoverable.length) {
			throw new Error(`interrupted integration cannot be restored: ${restored.unrecoverable.join(", ")}`);
		}
	}
	const patches = [{ relativePath: ".", patch: record.delta.rootPatch }, ...record.delta.nestedPatches].filter(entry =>
		entry.patch.trim(),
	);
	for (const entry of patches) {
		const target = path.resolve(root, entry.relativePath);
		if (target !== root && !target.startsWith(`${root}${path.sep}`)) {
			throw new Error(`integration patch escapes the run root: ${entry.relativePath}`);
		}
		if (!(await vcs.requireGit(target).canApplyPatch(entry.patch, {}))) {
			throw new Error(`integration conflict in ${entry.relativePath}; accepted patch preserved, delivery refused`);
		}
	}
	guard.begin();
	record.status = "applying";
	await writeRunState(journal, record);
	for (const entry of patches) {
		await vcs.requireGit(path.join(root, entry.relativePath)).applyPatch(entry.patch, {});
	}
	const settled = guard.settle({ protectedGlobs: [], patchDir: runDir });
	if (settled.unauthorized.length || settled.unrecoverable.length) {
		throw new Error("integration attempted to change protected workflow state; delivery refused");
	}
	if (verifyDelivered) {
		const delivered = await verifyDelivered(root);
		if (!delivered.ok) {
			// Denying every path reverts the whole apply: the boundary taken
			// above is still live, and the driver is the only writer here, so
			// nothing else can be caught in the rollback.
			const undo = guard.settle({ allowed: [], protectedGlobs: [], patchDir: runDir });
			record.status = "rejected";
			await writeRunState(journal, record);
			throw new Error(
				undo.unrecoverable.length
					? `${delivered.evidence}; revert incomplete, still applied: ${undo.unrecoverable.join(", ")}`
					: `${delivered.evidence}; landing reverted`,
			);
		}
	}
	record.status = "integrated";
	await writeRunState(journal, record);
}

export async function recoverIntegration(root: string, runDir: string, traceDir: string): Promise<void> {
	let record: IntegrationRecord;
	try {
		record = (await Bun.file(path.join(runDir, "integration.json")).json()) as IntegrationRecord;
	} catch (error) {
		if (isEnoent(error)) return;
		throw error;
	}
	if (record.status === "integrated" || record.status === "rejected") return;
	if (record.status !== "prepared" && record.status !== "applying") {
		throw new Error("unrecognized persisted integration state");
	}
	if (acceptedSubmission(traceDir, record)) await integrateAccepted(root, runDir, record);
}
