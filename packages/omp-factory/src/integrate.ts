/**
 * Serialized exactly-once integration (WP-07): a content-addressed journal.
 *
 * Reconciliation compares CONTENT, not a digest. A 64-bit non-cryptographic
 * hash decided "already applied" here before; the content it hashes is
 * written by an agent, so a collision was reachable by something with an
 * interest in reaching it, and the cost of a collision was a divergent file
 * silently skipped. The bytes are already in memory to hash them, so the
 * comparison that cannot be fooled is also the cheaper one.
 *
 * A crash in APPLYING reconciles forward when the intended content is
 * already present exactly, and refuses otherwise — never a blind re-apply.
 */
import { mkdir, rename, rm } from "node:fs/promises";
import path from "node:path";

// `reverted` is terminal and distinct from `prepared`: the landing happened
// and was undone, which a resume must not mistake for one that never ran.
export type IntegrationStatus = "prepared" | "applying" | "applied" | "committed" | "reverted";

export interface FileChange {
	path: string;
	/** Content at prepare time; `null` when the file did not exist. */
	before: string | null;
	/** Content this landing intends to leave behind. */
	after: string;
}

export interface IntegrationJournal {
	base: string;
	patchDigest: string;
	status: IntegrationStatus;
	changes: FileChange[];
}

/** Provenance, not an equality test — equality is decided on content. */
function digest(value: string): string {
	return new Bun.CryptoHasher("sha256").update(value).digest("hex").slice(0, 32);
}

async function readFile(root: string, rel: string): Promise<string | null> {
	try {
		return await Bun.file(path.join(root, rel)).text();
	} catch {
		return null;
	}
}

/** Capture before-state and intended after-state without touching the tree. */
export async function prepareIntegration(
	root: string,
	base: string,
	candidate: Record<string, string>,
): Promise<IntegrationJournal> {
	const changes: FileChange[] = [];
	for (const [rel, after] of Object.entries(candidate)) {
		changes.push({ path: rel, before: await readFile(root, rel), after });
	}
	const patchDigest = digest(JSON.stringify(changes.map(change => [change.path, change.after])));
	return { base, patchDigest, status: "prepared", changes };
}

async function persistJournal(dir: string, journal: IntegrationJournal): Promise<void> {
	await Bun.write(`${dir}/integration.json.tmp`, JSON.stringify(journal));
	await rename(`${dir}/integration.json.tmp`, `${dir}/integration.json`);
}

export async function loadJournal(dir: string): Promise<IntegrationJournal | null> {
	try {
		return (await Bun.file(`${dir}/integration.json`).json()) as IntegrationJournal;
	} catch {
		return null;
	}
}

/**
 * Replace one file atomically. An in-place write truncates first, so a crash
 * inside it leaves content matching neither before nor after — a state
 * reconciliation cannot resolve, which wedges every future recovery. Writing
 * beside the target and renaming leaves the file at exactly one of the two.
 */
async function writeAtomic(target: string, content: string): Promise<void> {
	const dir = path.dirname(target);
	await mkdir(dir, { recursive: true });
	const tmp = path.join(dir, `.${path.basename(target)}.omp-${Bun.randomUUIDv7()}`);
	await Bun.write(tmp, content);
	await rename(tmp, target);
}

/**
 * Apply once. Per file: already-after reconciles forward; still-before
 * applies; anything else refuses — the tree moved under us.
 */
export async function applyIntegration(
	root: string,
	journalDir: string,
	journal: IntegrationJournal,
): Promise<IntegrationJournal> {
	if (journal.status === "committed" || journal.status === "applied") return journal;
	if (journal.status !== "prepared" && journal.status !== "applying") {
		throw new Error(`refusing integration from status ${journal.status}`);
	}
	const applying: IntegrationJournal = { ...journal, status: "applying" };
	await persistJournal(journalDir, applying);
	for (const change of applying.changes) {
		const current = await readFile(root, change.path);
		if (current === change.after) continue;
		if (current !== change.before) {
			throw new Error(`refusing to integrate ${change.path}: tree diverged from prepared base`);
		}
		await writeAtomic(path.join(root, change.path), change.after);
		const written = await readFile(root, change.path);
		if (written !== change.after) {
			throw new Error(`integration of ${change.path} did not land exactly`);
		}
	}
	const applied: IntegrationJournal = { ...applying, status: "applied" };
	await persistJournal(journalDir, applied);
	return applied;
}

export async function commitIntegration(journalDir: string, journal: IntegrationJournal): Promise<IntegrationJournal> {
	if (journal.status !== "applied") {
		throw new Error(`cannot commit integration in status ${journal.status}`);
	}
	const committed: IntegrationJournal = { ...journal, status: "committed" };
	await persistJournal(journalDir, committed);
	return committed;
}

/**
 * Undo an applied-but-unverified landing.
 *
 * Reachable only between APPLIED and COMMITTED, where the journal still
 * holds each file's `before` content, so the restore is exact rather than a
 * guess. A file that no longer matches what this landing wrote is left
 * alone and reported: something else owns it now, and overwriting would
 * destroy that instead of repairing this.
 */
export async function revertIntegration(
	root: string,
	journalDir: string,
	journal: IntegrationJournal,
): Promise<{ reverted: string[]; skipped: string[] }> {
	if (journal.status !== "applied") {
		throw new Error(`cannot revert integration in status ${journal.status}`);
	}
	const reverted: string[] = [];
	const skipped: string[] = [];
	for (const change of journal.changes) {
		const current = await readFile(root, change.path);
		if (current !== change.after) {
			skipped.push(change.path);
			continue;
		}
		if (change.before === null) {
			await rm(path.join(root, change.path), { force: true });
		} else {
			await writeAtomic(path.join(root, change.path), change.before);
		}
		reverted.push(change.path);
	}
	await persistJournal(journalDir, { ...journal, status: "reverted" });
	return { reverted, skipped };
}

/** Recover after a crash: only APPLYING reconciles; anything else is returned as-is. */
export async function recoverIntegration(root: string, journalDir: string): Promise<IntegrationJournal | null> {
	const journal = await loadJournal(journalDir);
	if (!journal || journal.status !== "applying") return journal;
	return applyIntegration(root, journalDir, journal);
}
