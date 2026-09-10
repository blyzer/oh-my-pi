/**
 * I14: verifying the candidate is not verifying what gets delivered.
 *
 * An isolated writer is verified inside its own copy of the tree. The change
 * then lands somewhere else. When two writers each pass alone and conflict
 * only once both are on the shared root, nothing in the candidate path can
 * see it -- each sandbox was genuinely fine.
 */
import { afterEach, describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { type GraphPhase, runGraph } from "./graph";
import { copyIsolation } from "./isolation";

let root = "";
let runDir = "";
let journalDir = "";

async function makeDirs(): Promise<void> {
	root = await fs.mkdtemp(path.join(os.tmpdir(), "factory-delivered-root-"));
	runDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-delivered-run-"));
	journalDir = await fs.mkdtemp(path.join(os.tmpdir(), "factory-delivered-journal-"));
}

afterEach(async () => {
	for (const dir of [root, runDir, journalDir]) {
		if (dir) await fs.rm(dir, { recursive: true, force: true });
	}
	root = "";
	runDir = "";
	journalDir = "";
});

/**
 * Each writer creates its own file and gates on "at most one `claim-*` file
 * exists". Inside a sandbox that is always true: the copy is taken before
 * the sibling lands, so each writer only ever sees its own. On the shared
 * root both files coexist, and the same gate fails -- a conflict no
 * candidate check can observe, which is precisely what I14 is about.
 */
function conflicting(name: string): GraphPhase {
	const file = `claim-${name}.txt`;
	return {
		name,
		scope: ["**"],
		assertions: [],
		maxAttempts: 1,
		writes: true,
		dependsOn: [],
		// Fails once BOTH claims are present. In a sandbox only ever one is,
		// so every candidate passes its own gate; the shared root is the only
		// place the pair can be seen.
		gateCommand: ["sh", "-c", "! { test -f claim-alpha.txt && test -f claim-beta.txt; }"],
		produce: async ({ workspace }) => {
			await Bun.write(path.join(workspace, file), `${name}\n`);
			return { changedFiles: [file], declaredArtifacts: [file], label: name, exitCode: 0 };
		},
	};
}

describe("delivered-tree verification", () => {
	it("refuses a landing whose delivered tree fails the gate, and reverts it", async () => {
		await makeDirs();

		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			isolation: copyIsolation(),
			integrate: { journalDir, base: "delivered-base" },
			// Independent, so both sandboxes are copied from the same root
			// before either lands. Each writer's own gate passes; the shared
			// root only breaks once the second change-set arrives.
			phases: [conflicting("alpha"), conflicting("beta")],
		});
		// Which writer wins the landing lock is a race, so name neither: the
		// contract is that exactly one lands and the loser is reverted.
		expect(result.status).toBe("failed");
		const rejected = result.phases.filter(outcome => outcome.status === "rejected");
		const accepted = result.phases.filter(outcome => outcome.status === "accepted");
		expect(accepted).toHaveLength(1);
		expect(rejected).toHaveLength(1);
		expect(rejected[0]?.evidence.join(" ")).toContain("delivered tree failed the gate");

		// The revert is the half that makes this fail closed: the delivered
		// tree must hold the winner's claim and not the loser's.
		const winner = accepted[0]?.phase;
		const loser = rejected[0]?.phase;
		expect(await fs.exists(path.join(root, `claim-${winner}.txt`))).toBeTrue();
		expect(await fs.exists(path.join(root, `claim-${loser}.txt`))).toBeFalse();
	});

	it("commits when the delivered tree still passes", async () => {
		await makeDirs();

		const result = await runGraph({
			workflowId: "wf",
			runDir,
			workspace: root,
			isolation: copyIsolation(),
			integrate: { journalDir, base: "delivered-base" },
			phases: [conflicting("alpha")],
		});

		expect(result.status).toBe("accepted");
		expect(await fs.exists(path.join(root, "claim-alpha.txt"))).toBeTrue();
	});
});
