/**
 * Live crash drill: SIGKILL a real applier process mid-apply over thousands
 * of real files, then recover the journal and prove exact-once landing.
 * Exit nonzero on any deviation. No model involved — the crash mechanics
 * (journal + reconcile) are identical with or without one.
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import {
	applyIntegration,
	commitIntegration,
	loadJournal,
	prepareIntegration,
	recoverIntegration,
} from "../src/integrate";

const FILE_COUNT = 8000;

const pkgDir = path.resolve(import.meta.dir, "..");
const root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-drill-root-"));
const journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-drill-journal-"));
const after: Record<string, string> = {};
try {
	for (let i = 0; i < FILE_COUNT; i++) {
		const rel = `pkg/file_${i}.txt`;
		await Bun.write(path.join(root, rel), `before-${i}\n`);
		after[rel] = `${"x".repeat(1024)}after-${i}\n`;
	}
	const prepared = await prepareIntegration(root, "drill-base", after);
	await Bun.write(path.join(journalDir, "prepared.json"), JSON.stringify(prepared));
	await Bun.write(path.join(journalDir, "integration.json"), JSON.stringify({ ...prepared, status: "prepared" }));

	const child = Bun.spawn(
		["bun", "scripts/crash-applier.ts", root, journalDir, path.join(journalDir, "prepared.json")],
		{
			cwd: pkgDir,
			stdout: "pipe",
			stderr: "pipe",
		},
	);
	// Deterministic kill window: wait until the child persists APPLYING (its
	// loop has started over thousands of files), then kill mid-flight.
	const deadline = Date.now() + 30_000;
	let sawApplying = false;
	while (Date.now() < deadline) {
		const current = await loadJournal(journalDir);
		if (current?.status === "applying") {
			sawApplying = true;
			break;
		}
		if (current?.status === "applied" || current?.status === "committed") break;
		await Bun.sleep(25);
	}
	if (!sawApplying) throw new Error("applier never reached APPLYING; no crash window");
	await Bun.sleep(150);
	child.kill(9);
	await child.exited.catch(() => undefined);

	const recovered = await recoverIntegration(root, journalDir);
	if (!recovered) throw new Error("no journal to recover");
	const applied = recovered.status === "prepared" ? await applyIntegration(root, journalDir, recovered) : recovered;
	if (applied.status !== "applied") throw new Error(`recovery ended ${applied.status}, not applied`);
	const committed = await commitIntegration(journalDir, applied);

	let bad = 0;
	for (const [rel, content] of Object.entries(after)) {
		if ((await Bun.file(path.join(root, rel)).text()) !== content) bad += 1;
	}
	if (bad > 0) throw new Error(`${bad} files differ from intended content`);
	const journal = await loadJournal(journalDir);
	if (committed.status !== "committed" || journal?.status !== "committed") {
		throw new Error("journal did not reach committed");
	}
	console.log(`CRASH-DRILL-OK files=${FILE_COUNT} journal=${journal.status}`);
} finally {
	await fs.rm(root, { recursive: true, force: true });
	await fs.rm(journalDir, { recursive: true, force: true });
}
