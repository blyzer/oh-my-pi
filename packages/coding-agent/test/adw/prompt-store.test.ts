import { describe, expect, it } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { ENVELOPE_CONTRACT_BUNDLED, envelopeContract } from "../../src/adw/prompt";
import {
	describePromptOrigin,
	loadPrompt,
	parsePromptFile,
	PROMPT_DIR,
	promptDigest,
	resetPromptCache,
} from "../../src/adw/prompt-store";

function workspace(override?: string): string {
	const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "prompt-store-"));
	if (override !== undefined) {
		const dir = path.join(cwd, PROMPT_DIR);
		fs.mkdirSync(dir, { recursive: true });
		fs.writeFileSync(path.join(dir, "envelope-contract.md"), override);
	}
	resetPromptCache();
	return cwd;
}

describe("parsePromptFile", () => {
	it("keeps frontmatter out of the prompt body", () => {
		// Frontmatter describes the file for a human reader. Leaving it in the
		// text would ship "description:" to the model as an instruction.
		const parsed = parsePromptFile(
			'---\ndescription: "what this is"\nsource: "adapted from X"\n---\n\nThe actual prompt.\n',
		);
		expect(parsed.text).toBe("The actual prompt.");
		expect(parsed.frontmatter.description).toBe("what this is");
		expect(parsed.frontmatter.source).toBe("adapted from X");
	});

	it("treats a file without frontmatter as all body", () => {
		expect(parsePromptFile("Just the prompt.\n").text).toBe("Just the prompt.");
	});
});

describe("loadPrompt", () => {
	it("uses the bundled text when no override exists", () => {
		const cwd = workspace();
		expect(envelopeContract(cwd)).toBe(ENVELOPE_CONTRACT_BUNDLED);
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("prefers an override that supplies a body", () => {
		const cwd = workspace("---\ndescription: mine\n---\n\nEnd with JSON.\n");
		expect(envelopeContract(cwd)).toBe("End with JSON.");
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("falls back to bundled text when the override is empty", () => {
		// A file containing only frontmatter leaves the engine with no prompt
		// at all. Failing the run would let a typo in an optional file break a
		// workflow that never asked for the override.
		const cwd = workspace("---\ndescription: mine\n---\n");
		expect(envelopeContract(cwd)).toBe(ENVELOPE_CONTRACT_BUNDLED);
		fs.rmSync(cwd, { recursive: true, force: true });
	});

	it("reports where the text came from", () => {
		// Provenance is the point of moving prompts to files: an operator
		// reading a trace has to be able to tell which text produced it.
		const cwd = workspace("Overridden.\n");
		const loaded = loadPrompt("envelope-contract", ENVELOPE_CONTRACT_BUNDLED, cwd);
		expect(loaded.path).toBe(path.join(cwd, PROMPT_DIR, "envelope-contract.md"));

		const plain = workspace();
		expect(loadPrompt("envelope-contract", ENVELOPE_CONTRACT_BUNDLED, plain).path).toBeUndefined();

		fs.rmSync(cwd, { recursive: true, force: true });
		fs.rmSync(plain, { recursive: true, force: true });
	});

	it("does not re-read an override mid-process", () => {
		// Two attempts of one phase under different instructions, both recorded
		// against one trace, makes the trace a lie.
		const cwd = workspace("First wording.\n");
		expect(envelopeContract(cwd)).toBe("First wording.");
		fs.writeFileSync(path.join(cwd, PROMPT_DIR, "envelope-contract.md"), "Second wording.\n");
		expect(envelopeContract(cwd)).toBe("First wording.");
		fs.rmSync(cwd, { recursive: true, force: true });
	});
});

describe("prompt provenance", () => {
	it("gives identical text the same digest and different text a different one", () => {
		// The trace records the digest, not the prompt: what a reader needs is
		// whether two attempts ran under the same instructions.
		expect(promptDigest("End with JSON.")).toBe(promptDigest("End with JSON."));
		expect(promptDigest("End with JSON.")).not.toBe(promptDigest("End with JSON. Please."));
	});

	it("names the origin, not just the digest", () => {
		// A digest alone tells a reader two runs differed without telling them
		// where to look. Bundled versus a path is the first question they ask.
		expect(describePromptOrigin("envelope-contract", { text: "x", frontmatter: {} })).toContain("bundled");
		const override = describePromptOrigin("classifier", {
			text: "y",
			frontmatter: {},
			path: path.join(process.cwd(), PROMPT_DIR, "classifier.md"),
		});
		expect(override).toContain("classifier.md");
		expect(override).not.toContain("bundled");
	});
});
