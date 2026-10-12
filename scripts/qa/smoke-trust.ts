#!/usr/bin/env bun
// Real terminal acceptance; no provider calls or developer configuration.
// CARGO_TARGET_DIR=/path/to/target bun scripts/qa/smoke-trust.ts
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import assert from "node:assert/strict";
import tuiFactory, { type TuiParams } from "../../.omp/tools/tui.ts";

const repo = resolve(import.meta.dir, "../..");
const target = resolve(process.env.CARGO_TARGET_DIR ?? join(repo, "target"));
const root = realpathSync(mkdtempSync(join(tmpdir(), "omp-trust-pty-")));
const marker = "TRUST_OVERLAY_SYSTEM_MARKER";
const schema = { describe() { return this; }, optional() { return this; } };
const zod = { object: () => schema, string: () => schema, boolean: () => schema, number: () => schema, array: () => schema };

function fixture(name: string, gated = true) {
	const dir = join(root, name);
	const project = join(dir, "team/project");
	const home = join(dir, "home");
	const config = join(dir, "config");
	const data = join(dir, "data");
	for (const path of [project, home, config, data]) mkdirSync(path, { recursive: true });
	symlinkSync(target, join(project, "target"), "dir");
	if (gated) {
		mkdirSync(join(project, ".omp"));
		writeFileSync(join(project, ".omp/SYSTEM.md"), marker);
	}
	return { dir, project, home, config, data, grants: join(data, "ext/grants.toml") };
}

interface Fixture {
	dir: string;
	project: string;
	home: string;
	config: string;
	data: string;
	grants: string;
}
async function run(f: Fixture, name: string, keys: string | null, trusted: boolean, changed = false) {
	Object.assign(process.env, { HOME: f.home, OMP_CONFIG_DIR: f.config, OMP_DATA_DIR: f.data, OMP_LLM_KEY_SOURCE: "local-file" });
	for (const key of ["OMP_CONFIG_FILES", "OMP_PROFILE", "OMP_CODING_AGENT_SESSION_DIR", "OMP_NO_PTY"]) delete process.env[key];
	const sessions = join(f.dir, name);
	const tool = tuiFactory({ cwd: f.project, zod, async exec() { throw new Error("proof requires an already built omp binary"); } });
	async function op(params: TuiParams) {
		const result = await tool.execute("trust-proof", { name, ...params });
		return result.content.filter(part => part.type === "text").map(part => part.text).join("\n");
	}
	async function waitFor(test: (text: string) => boolean, description: string) {
		const deadline = Date.now() + 120_000;
		let text = "";
		while (Date.now() < deadline) {
			text = await op({ op: "screen" });
			if (test(text)) return text;
			await Bun.sleep(100);
		}
		throw new Error(`${name}: ${description}\n${text}`);
	}
	console.log(await op({ op: "start", bin: "omp", build: false, args: ["chat", "--no-ext", "--no-title", "--model", "openai/gpt-5", "--session-dir", sessions], rows: 35, cols: 110, timeout: 30 }));
	try {
		if (keys !== null) {
			const screen = await waitFor(text => text.includes("Do not trust"), "trust overlay missing");
			assert(screen.includes(".omp/SYSTEM.md"));
			assert.equal(screen.includes("inputs changed"), changed);
			console.log(`\n===== ${name}: default selection =====\n${screen}`);
			await op({ op: "resize", rows: 30, cols: 95, quiet: true });
			await waitFor(text => text.includes("Do not trust"), "resize lost overlay");
			await op({ op: "send", text: keys, quiet: true });
		}
		const chat = await waitFor(text => text.includes("Ask anything"), "chat failed to compose");
		assert(!chat.includes("Do not trust"), "overlay must not remain over chat");
		console.log(`\n===== ${name}: composed =====\n${chat}`);
		const journals = readdirSync(sessions, { recursive: true }).map(String).filter(path => path.endsWith(".oms"));
		assert.equal(journals.length, 1, "one durable session");
		const journal = readFileSync(join(sessions, journals[0]), "utf8");
		assert(journal.includes("workspace-trust"), "composition must journal its trust decision");
		assert.equal(journal.includes(marker), trusted, "SYSTEM.md must load only after approval");
		assert(journal.includes(trusted ? '"decision":"trusted"' : '"decision":"untrusted"'), "journal decision must match selection");
		await op({ op: "send", text: "\\x03\\x03", quiet: true });
		const deadline = Date.now() + 15_000;
		let listing = "";
		while (Date.now() < deadline) {
			listing = await op({ op: "list" });
			if (listing.includes("exited(0)")) break;
			await Bun.sleep(100);
		}
		assert(listing.includes("exited(0)"), `unclean exit: ${listing}`);
		const raw = await op({ op: "raw", peek: 0 });
		console.log(`===== ${name}: terminal restoration =====\n${raw}`);
		const stats = JSON.parse(raw.split("\n")[0]);
		assert((stats.mouse_on ?? 0) <= (stats.mouse_off ?? 0), "mouse tracking leaked");
		assert((stats.alt_on ?? 0) <= (stats.alt_off ?? 0), "alternate buffer leaked");
	} finally {
		await op({ op: "stop" });
	}
}

console.log(`Evidence directory: ${root}`);
for (const [name, keys] of [["default", "\\r"], ["escape", "\\x1b"], ["ctrl-c", "\\x03"]]) {
	const f = fixture(name);
	await run(f, name, keys, false);
	assert(!existsSync(f.grants), "denial must not persist a grant");
}
const session = fixture("session");
await run(session, "session", "\\x1b[B\\r", true);
assert(!existsSync(session.grants), "session trust must not persist");
const persistent = fixture("persistent");
await run(persistent, "persistent", "\\x1b[B\\x1b[B\\r", true);
assert(readFileSync(persistent.grants, "utf8").includes('scope = "exact"'), "workspace trust must persist exact grant");
await run(persistent, "trusted-restart", null, true);
writeFileSync(join(persistent.project, ".omp/SYSTEM.md"), `${marker}\nchanged`);
await run(persistent, "digest-changed", "\\x1b[B\\x1b[B\\r", true, true);
const pin = fixture("pin");
mkdirSync(join(pin.data, "ext"));
writeFileSync(pin.grants, `[[workspace_trust]]\nworkspace = ${JSON.stringify(join(pin.dir, "team"))}\nscope = "subtree"\ngranted_at = "2026-10-12T00:00:00Z"\ngranted_by = "cli"\nduration = "persistent"\n`);
await run(pin, "pin", "\\x1b[B\\x1b[B\\x1b[B\\r", true);
assert(readFileSync(pin.grants, "utf8").includes('scope = "pin"'), "subtree approval must persist a pin");
await run(fixture("empty", false), "empty", null, false);
console.log("Trust overlay real-PTY acceptance passed");
