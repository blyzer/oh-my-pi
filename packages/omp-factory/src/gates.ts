/** Deterministic gates: executable code decides pass/fail; models cannot override. */

export type FileAssertion =
	| { type: "file_contains"; file: string; marker: string }
	| { type: "file_not_contains"; file: string; marker: string }
	| { type: "json_parses"; file: string }
	| { type: string; file?: string; marker?: string };

const KNOWN_TYPES = new Set(["file_contains", "file_not_contains", "json_parses"]);

export interface AssertionReport {
	passed: boolean;
	failures: string[];
}

/** Evaluate file-content assertions against a workspace root. Unknown types fail closed. */
export async function evaluateFileAssertions(assertions: FileAssertion[], root: string): Promise<AssertionReport> {
	const failures: string[] = [];
	for (const assertion of assertions) {
		if (!KNOWN_TYPES.has(assertion.type)) {
			failures.push(`unknown assertion type: ${assertion.type}`);
			continue;
		}
		if (!assertion.file) {
			failures.push(`malformed assertion: ${JSON.stringify(assertion)}`);
			continue;
		}
		const marker = "marker" in assertion ? assertion.marker : undefined;
		if (assertion.type !== "json_parses" && marker === undefined) {
			failures.push(`malformed assertion: ${JSON.stringify(assertion)}`);
			continue;
		}
		let content: string;
		try {
			content = await Bun.file(`${root}/${assertion.file}`).text();
		} catch {
			failures.push(`missing artifact: ${assertion.file}`);
			continue;
		}
		if (assertion.type === "json_parses") {
			// Bytes are not structure: a truncated plan.json or an apology in
			// place of an object satisfies existence and non-emptiness alike.
			// The parse position is what makes the failure actionable.
			try {
				JSON.parse(content);
			} catch (error) {
				failures.push(`${assertion.file} is not valid JSON: ${(error as Error).message}`);
			}
			continue;
		}
		const contains = marker !== undefined && content.includes(marker);
		if (assertion.type === "file_contains" && !contains) {
			failures.push(`${assertion.file} must contain ${JSON.stringify(marker)}`);
		}
		if (assertion.type === "file_not_contains" && contains) {
			failures.push(`${assertion.file} must not contain ${JSON.stringify(marker)}`);
		}
	}
	return { passed: failures.length === 0, failures };
}

export interface CommandVerdict {
	exitCode: number;
	output: string;
}

/** Run a gate command; nonzero exit (or crash/timeout) is failure. No model involved. */
export async function runGateCommand(command: string[], cwd: string, timeoutMs = 60_000): Promise<CommandVerdict> {
	const child = Bun.spawn(command, { cwd, stdout: "pipe", stderr: "pipe" });
	const timeout = setTimeout(() => {
		try {
			child.kill();
		} catch {
			// Already exited; verdict below reports the real outcome.
		}
	}, timeoutMs);
	try {
		const [stdout, stderr, exitCode] = await Promise.all([
			new Response(child.stdout).text(),
			new Response(child.stderr).text(),
			child.exited,
		]);
		const output = `${stdout}${stderr}`.slice(-4000);
		return { exitCode: exitCode ?? 124, output: exitCode === null ? `${output}\n[gate timed out]` : output };
	} finally {
		clearTimeout(timeout);
	}
}
