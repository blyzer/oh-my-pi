// `tui` tool `stop` contract: it ends a host through the host's own quit
// path, including hosts that quit only on a repeated `C-c` (`omp chat`) and
// hosts that close only on the debug `quit` op (the native host behind a
// modal overlay), and SIGKILLs only a host that ignores both. Each case
// spawns `fake-host.ts` on the tool's real Bun PTY through the public `start`
// op.
//
// Local only (CI is the Cargo gate): `just tools-test` installs the tool's
// dependencies and runs this file.

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

/** The text of a tool result's first content part; empty for an image. */
function textOf(result: Awaited<ReturnType<typeof tool.execute>>): string {
	const [part] = result.content;
	return part.type === "text" ? part.text : "";
}

/**
 * Polls the emulated screen until the fake host prints its readiness line,
 * which it does only after raw mode is on. Before that the PTY is cooked and
 * a raw `\x03` is SIGINT, not a `C-c` byte the host reads.
 */
async function waitForReady(name: string) {
	const deadline = Date.now() + 10_000;
	for (;;) {
		const screen = textOf(await tool.execute("screen", { op: "screen", name }));
		if (screen.includes("fake host ready")) return;
		if (Date.now() >= deadline) {
			throw new Error(`"${name}" never printed its readiness line:\n${screen}`);
		}
		await Bun.sleep(50);
	}
}

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
	await waitForReady(name);
	const stopped = await tool.execute("stop", { op: "stop", name });
	return textOf(stopped);
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

test("stop closes a host through the debug quit op when C-c does not", async () => {
	expect(await startThenStop("quit-op", ["quit-op"])).toBe(
		'stopped "quit-op" (exit 0)',
	);
}, 15_000);

test("stop SIGKILLs a host that ignores C-c and the quit op", async () => {
	expect(await startThenStop("never", ["never"])).toBe(
		'stopped "never" (exit -9)',
	);
}, 15_000);
