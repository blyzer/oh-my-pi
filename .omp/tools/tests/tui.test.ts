// `tui` tool `stop` contract: it ends a host through the host's own quit
// path, including hosts that quit only on a repeated `C-c` (`omp chat`), and
// SIGKILLs only a host that ignores the quit chord. Each case spawns
// `fake-host.ts` on the tool's real Bun PTY through the public `start` op.
//
// Needs the tool's dependencies (`bun install` in `.omp/tools`); run with
// `bun test` from `.omp/tools`.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import factory from "../tui.ts";

type Host = Parameters<typeof factory>[0];
type Schema = ReturnType<Host["zod"]["string"]>;

const schema: Schema = { describe: () => schema, optional: () => schema };
const zod: Host["zod"] = {
	object: () => schema,
	string: () => schema,
	boolean: () => schema,
	number: () => schema,
	array: () => schema,
};

let cwd = "";
let tool: ReturnType<typeof factory>;

beforeAll(() => {
	cwd = mkdtempSync(join(tmpdir(), "omp-tui-test-"));
	const bin = join(cwd, "target", "debug");
	mkdirSync(bin, { recursive: true });
	// `start` runs `<cwd>/target/debug/<bin>`; the launcher execs the fake
	// host under the Bun running this test.
	const launcher = join(bin, "fake-host");
	writeFileSync(
		launcher,
		`#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(
			join(import.meta.dir, "fake-host.ts"),
		)} "$@"\n`,
	);
	chmodSync(launcher, 0o755);
	tool = factory({
		cwd,
		zod,
		exec: () => Promise.reject(new Error("tests start with build: false")),
	});
});

afterAll(() => {
	rmSync(cwd, { recursive: true, force: true });
});

/** Starts the fake host as session `name`, stops it, and returns `stop`'s report. */
async function startThenStop(
	name: string,
	args: string[],
	socketWaitSeconds?: number,
): Promise<string> {
	await tool.execute("start", {
		op: "start",
		name,
		bin: "fake-host",
		args,
		build: false,
		timeout: socketWaitSeconds,
	});
	const stopped = await tool.execute("stop", { op: "stop", name });
	const [report] = stopped.content;
	return report.type === "text" ? report.text : "";
}

test("stop quits a debug-socket host that needs a repeated C-c", async () => {
	expect(await startThenStop("repeat", ["repeat"])).toBe(
		'stopped "repeat" (exit 0)',
	);
}, 15_000);

test("stop quits a socketless app that needs a repeated C-c", async () => {
	expect(await startThenStop("raw-repeat", ["repeat", "no-socket"], 0.5)).toBe(
		'stopped "raw-repeat" (exit 0)',
	);
}, 15_000);

test("stop quits a host that quits on the first C-c", async () => {
	expect(await startThenStop("first", ["first"])).toBe(
		'stopped "first" (exit 0)',
	);
}, 15_000);

test("stop SIGKILLs a host that ignores C-c", async () => {
	expect(await startThenStop("never", ["never"])).toBe(
		'stopped "never" (exit -9)',
	);
}, 15_000);
