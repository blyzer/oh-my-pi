/**
 * Serialized exactly-once-ish integration (WP-07): content-addressed journal.
 * A crash in APPLYING reconciles forward when the intended content is already
 * present exactly, and refuses otherwise — never blind re-apply.
 */

export type IntegrationStatus = "prepared" | "applying" | "applied" | "committed";

export interface FileChange {
	path: string;
	beforeHash: string | null;
	afterContent: string;
	afterHash: string;
}

export interface IntegrationJournal {
	base: string;
	patchDigest: string;
	status: IntegrationStatus;
	changes: FileChange[];
}

function hash(content: string): string {
	return Bun.hash(content).toString(16);
}

async function fileHash(root: string, rel: string): Promise<string | null> {
	try {
		return hash(await Bun.file(`${root}/${rel}`).text());
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
	for (const [rel, afterContent] of Object.entries(candidate)) {
		const beforeHash = await fileHash(root, rel);
		changes.push({ path: rel, beforeHash, afterContent, afterHash: hash(afterContent) });
	}
	const patchDigest = hash(JSON.stringify(changes.map(c => [c.path, c.beforeHash, c.afterHash])));
	return { base, patchDigest, status: "prepared", changes };
}

async function persistJournal(dir: string, journal: IntegrationJournal): Promise<void> {
	await Bun.write(`${dir}/integration.json.tmp`, JSON.stringify(journal));
	const { rename } = await import("node:fs/promises");
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
		const current = await fileHash(root, change.path);
		if (current === change.afterHash) continue;
		if (current !== change.beforeHash) {
			throw new Error(`refusing to integrate ${change.path}: tree diverged from prepared base`);
		}
		await Bun.write(`${root}/${change.path}`, change.afterContent);
		const written = await fileHash(root, change.path);
		if (written !== change.afterHash) {
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

/** Recover after a crash: only APPLYING reconciles; anything else is returned as-is. */
export async function recoverIntegration(root: string, journalDir: string): Promise<IntegrationJournal | null> {
	const journal = await loadJournal(journalDir);
	if (!journal || journal.status !== "applying") return journal;
	return applyIntegration(root, journalDir, journal);
}
