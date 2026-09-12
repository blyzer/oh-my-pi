/**
 * §15 baseline-aware verification: a bug reproducer must be RED before the
 * fix, or nothing proves it exercises the fault.
 *
 * The subtle half is not the inversion — it is what inversion must NOT
 * cover. A timeout, a cancellation, a signal or a failed spawn all produce
 * "did not exit 0", and treating those as "failed as required" would accept
 * a reproducer that never executed: the same hole this feature closes,
 * reopened from the other side.
 */
import { describe, expect, it } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { AdwPhaseConfig } from "../../src/adw/types";
import { parseWorkflow } from "../../src/adw/config";
import { type CodePhaseResult, judgeExpectation, runCodePhase } from "../../src/adw/runner";

const phase = (expectation: "pass" | "fail" | undefined, command: string): AdwPhaseConfig =>
	({ name: "verify", kind: "code", owner: "sh", command, expect: expectation }) as AdwPhaseConfig;

const ran = (ok: boolean, exitCode: number): CodePhaseResult => ({
	kind: "ran",
	ok,
	exitCode,
	summary: "output",
});
const infrastructure: CodePhaseResult = { kind: "infrastructure", ok: false, summary: "was killed after 100ms" };

describe("expect: fail", () => {
	it("accepts a command that ran and failed, naming the exit code", () => {
		const [accepted, summary] = judgeExpectation(phase("fail", "false"), ran(false, 1));
		expect(accepted).toBeTrue();
		// The trace must say why a red command counts as a pass, or a reader
		// seeing `ok: true` against a failure thinks the engine is confused.
		expect(summary).toContain("expected to fail and did");
		expect(summary).toContain("exit 1");
	});

	it("refuses a reproducer that passes: it does not exercise the fault", () => {
		const [accepted, summary] = judgeExpectation(phase("fail", "true"), ran(true, 0));
		expect(accepted).toBeFalse();
		expect(summary).toContain("does not exercise the fault");
	});

	it("refuses a reproducer that never ran, however it died", () => {
		// The decisive case. `ok` is false here exactly as it is for a red
		// test, and only the discriminant separates them.
		const [accepted, summary] = judgeExpectation(phase("fail", "sleep 99"), infrastructure);
		expect(accepted).toBeFalse();
		expect(summary).toContain("never ran");
	});

	it("leaves an ordinary gate alone in both directions", () => {
		expect(judgeExpectation(phase(undefined, "true"), ran(true, 0))[0]).toBeTrue();
		expect(judgeExpectation(phase(undefined, "false"), ran(false, 1))[0]).toBeFalse();
		expect(judgeExpectation(phase("pass", "false"), ran(false, 1))[0]).toBeFalse();
		// An ordinary gate is unaffected by infrastructure too: it was
		// already a failure, and stays one.
		expect(judgeExpectation(phase(undefined, "x"), infrastructure)[0]).toBeFalse();
	});
});

describe("runCodePhase result shape", () => {
	it("reports a real exit code as `ran`, and a timeout as infrastructure", async () => {
		// Pinned end to end, because the discriminant is only trustworthy if
		// the runner actually sets it — a test against a hand-built object
		// would pass even if the runner collapsed both into one shape.
		const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "expect-"));

		const failed = await runCodePhase(phase("fail", "exit 3"), cwd, undefined);
		expect(failed.kind).toBe("ran");
		if (failed.kind === "ran") expect(failed.exitCode).toBe(3);

		const killed = await runCodePhase(
			{ ...phase("fail", "sleep 30"), timeoutMs: 200 } as AdwPhaseConfig,
			cwd,
			undefined,
		);
		expect(killed.kind).toBe("infrastructure");
		// And therefore refused, rather than counted as a satisfied MUST_FAIL.
		expect(judgeExpectation(phase("fail", "sleep 30"), killed)[0]).toBeFalse();

		fs.rmSync(cwd, { recursive: true, force: true });
	});
});

describe("expect: fail is validated as a pair", () => {
	const workflow = (phases: string[]): string => ["name: bugfix", "phases:", ...phases].join("\n");
	const red = [
		"  - name: red",
		"    kind: code",
		"    owner: bun",
		'    command: "bun test repro"',
		"    expect: fail",
	];
	const green = ["  - name: green", "    kind: code", "    owner: bun", '    command: "bun test repro"'];
	const fix = ["  - name: fix", "    kind: agent", "    owner: task", '    writes: ["src/**"]'];

	it("accepts a serial red -> fix -> green, where the edges are implicit", () => {
		// The declaration-order edge is a real dependency, so requiring an
		// explicit `dependsOn` here would reject the most ordinary shape a
		// bugfix workflow takes.
		expect(() => parseWorkflow("w.yml", workflow([...red, ...fix, ...green]))).not.toThrow();
	});

	it("accepts an explicitly wired pair", () => {
		expect(() =>
			parseWorkflow("w.yml", workflow([...red, ...fix, "    dependsOn: [red]", ...green, "    dependsOn: [fix]"])),
		).not.toThrow();
	});

	it("refuses a red reproducer with no green counterpart", () => {
		// Proving the bug and never re-checking proves nothing about the fix.
		expect(() => parseWorkflow("w.yml", workflow([...red, ...fix]))).toThrow(/never the fix/);
	});

	it("refuses a green that does not depend on the red", () => {
		// Declared the other way round, the implementation lands before the
		// bug was ever demonstrated.
		expect(() =>
			parseWorkflow(
				"w.yml",
				workflow([...green, "    dependsOn: []", ...fix, "    dependsOn: []", ...red, "    dependsOn: [fix]"]),
			),
		).toThrow(/never the fix/);
	});

	it("refuses a pair with nothing between them that could change the tree", () => {
		// One command asserted to both fail and pass on an unchanged tree is
		// a flaky test, not a repair.
		expect(() => parseWorkflow("w.yml", workflow([...red, ...green]))).toThrow(/nothing between them/);
	});

	it("does not count a phase that cannot write as the fix", () => {
		// `writes: []` denies every path and `human` answers a question:
		// neither can be what turns the red run green, so neither satisfies
		// the pairing.
		const readOnly = ["  - name: noop", "    kind: agent", "    owner: task", "    writes: []"];
		const asks = ["  - name: ask", "    kind: human", "    description: proceed?"];
		expect(() => parseWorkflow("w.yml", workflow([...red, ...readOnly, ...green]))).toThrow(/nothing between them/);
		expect(() => parseWorkflow("w.yml", workflow([...red, ...asks, ...green]))).toThrow(/nothing between them/);
	});

	it("refuses a reproducer whose command does not exist or cannot run", async () => {
		// 127 is "command not found", 126 "not executable": the shell ran,
		// the reproducer did not. Counting either as red lets a typo satisfy
		// MUST_FAIL.
		//
		// Runs the real commands rather than hand-building the result: a test
		// on a literal would stay green if the runner stopped classifying
		// these, which is the regression worth catching.
		const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "expect-exit-"));
		const notExecutable = path.join(cwd, "not-executable.sh");
		fs.writeFileSync(notExecutable, "#!/bin/sh\nexit 1\n", { mode: 0o644 });

		// The mirror cases. An exit status is the command's to choose, so no
		// status and no stderr text can prove the command did not run --
		// only the filesystem can, and only before execution. Each of these
		// ran and must count as a verdict.
		const ambiguous = path.join(cwd, "ambiguous.sh");
		// Prints the shell's own not-found wording AND exits 127: defeats
		// both a status check and a stderr match.
		fs.writeFileSync(ambiguous, '#!/bin/sh\necho "foo: command not found" >&2\nexit 127\n', { mode: 0o755 });
		for (const [command, why] of [
			[ambiguous, "a script that printed the shell's wording and exited 127"],
			["exit 143", "an exit status that looks like SIGTERM"],
			["exit 1", "a builtin, which resolves to no file at all"],
		] as const) {
			const legitimate = phase("fail", command);
			const result = await runCodePhase(legitimate, cwd, undefined);
			expect(result.kind, `${why} is a verdict`).toBe("ran");
		}

		for (const command of ["no-such-command-xyz", notExecutable]) {
			const typo = phase("fail", command);
			const result = await runCodePhase(typo, cwd, undefined);
			expect(result.kind, `${command} classified as ${result.kind}`).toBe("infrastructure");
			expect(judgeExpectation(typo, result)[0]).toBeFalse();
		}
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("refuses a reproducer whose interpreter is missing", async () => {
		// Executable, and still unable to start: the shebang names an
		// interpreter that is not there, and the shell reports 127 exactly as
		// it would for a missing command. A filesystem probe passes this
		// file; only launching it reveals the failure.
		const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "expect-shebang-"));
		const script = path.join(cwd, "bad.sh");
		fs.writeFileSync(script, "#!/no/such/interpreter\necho hi\n", { mode: 0o755 });
		const broken = phase("fail", script);
		const result = await runCodePhase(broken, cwd, undefined);
		expect(result.kind, "a script whose interpreter is missing never ran").toBe("infrastructure");
		expect(judgeExpectation(broken, result)[0]).toBeFalse();
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("runs the command exactly once", async () => {
		// The launch check must not be a second execution: a reproducer with
		// a side effect would perform it before the workflow judged anything.
		const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "expect-once-"));
		const marker = path.join(cwd, "runs");
		const script = path.join(cwd, "fx.sh");
		fs.writeFileSync(script, `#!/bin/sh\nprintf x >> ${marker}\nexit 1\n`, { mode: 0o755 });
		await runCodePhase(phase("fail", script), cwd, undefined);
		expect(fs.readFileSync(marker, "utf8")).toBe("x");
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("refuses expect: fail on a command the shell must interpret", () => {
		// The guarantee rests on launching the command directly. A chain or a
		// pipeline has no single executable, so `no-such-command && false`
		// would reach the shell and return 127 -- the same status a real
		// failure gives. Refused at load rather than documented as an
		// exception.
		for (const command of ["no-such-command && false", "no-such-xyz | cat", "bun test > out.txt"]) {
			const phases = [
				"  - name: red",
				"    kind: code",
				"    owner: bun",
				`    command: "${command}"`,
				"    expect: fail",
				...fix,
				"  - name: green",
				"    kind: code",
				"    owner: bun",
				`    command: "${command}"`,
			];
			expect(() => parseWorkflow("w.yml", workflow(phases)), command).toThrow(/not a plain command/);
		}
	});

	it("runs both halves of the pair the same way", () => {
		// A pair only repeats the same test if both halves launch it
		// identically. RED runs direct argv so a failed exec stays
		// distinguishable from a failing test; a GREEN going through the
		// shell would reintroduce quoting and word-splitting differences
		// between two runs the workflow claims are the same.
		const parsed = parseWorkflow("w.yml", workflow([...red, ...fix, ...green]));
		const greenPhase = parsed.phases.find(phase => phase.name === "green");
		expect(greenPhase?.directLaunch, "green counterpart launches like red").toBe(true);
	});

	it("refuses expect on a phase that has no exit code", () => {
		expect(() =>
			parseWorkflow("w.yml", workflow(["  - name: plan", "    kind: agent", "    owner: task", "    expect: fail"])),
		).toThrow(/only applies to code phases/);
	});
});

describe("the shipped tdd.yml example", () => {
	it("parses, so the template that teaches this feature is not itself broken", async () => {
		// A doc example that does not load teaches nothing: the first thing a
		// user does is copy it into .omp/adw/ and run it.
		const text = await Bun.file(`${import.meta.dir}/../../../../docs/adw/examples/tdd.yml`).text();
		const workflow = parseWorkflow("tdd.yml", text);
		expect(workflow.phases.map(phase => phase.name)).toEqual(["reproduce", "red", "fix", "green"]);
		// And it exercises the guarantee: red and green share a command, with
		// a writer between them.
		const red = workflow.phases.find(phase => phase.name === "red");
		const green = workflow.phases.find(phase => phase.name === "green");
		expect(red?.expect).toBe("fail");
		expect(green?.command).toBe(red?.command);
	});
});
