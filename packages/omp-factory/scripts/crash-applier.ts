/**
 * Crash-drill child: applies a prepared journal. Expects SIGKILL mid-flight;
 * never catches signals, never reconciles — that is the parent's job.
 */
import { applyIntegration, type IntegrationJournal } from "../src/integrate";

const [root, journalDir, journalFile] = Bun.argv.slice(2);
if (!root || !journalDir || !journalFile) {
	console.error("usage: crash-applier.ts <root> <journalDir> <preparedJournalFile>");
	process.exit(2);
}

const journal = (await Bun.file(journalFile).json()) as IntegrationJournal;
const applied = await applyIntegration(root, journalDir, journal);
console.log(`applier survived: ${applied.status} (${applied.changes.length} files)`);
