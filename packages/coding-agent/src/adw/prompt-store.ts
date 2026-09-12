/**
 * Prompts as files the operator can read, diff and override.
 *
 * A prompt buried in a template literal is invisible to the people most
 * affected by it: it does not appear in a review as prose, an operator cannot
 * try a wording without a rebuild, and a coding agent asked to improve a
 * workflow cannot find the text that shapes its own behaviour.
 *
 * So each built-in prompt also has a file. The bundled text stays in the
 * source as the default — a missing or malformed override must never leave
 * the engine without a prompt — and a file in `.omp/adw/prompts/` replaces it
 * when present.
 *
 * Overrides are read once per process. A workflow that re-read them mid-run
 * could dispatch two attempts of the same phase under different instructions
 * and record both against one trace, which makes the trace a lie.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { logger } from "@oh-my-pi/pi-utils";

/**
 * Prompts the engine supplies itself, as opposed to a phase's own `prompt:`.
 *
 * `review-rules` is deliberately absent. That text states what `reviewSchema`
 * actually enforces, so an override could only make the prompt disagree with
 * the validator — telling a reviewer a rule that the engine then refuses, or
 * hiding one it applies anyway. Prompts that describe engine behaviour belong
 * to the engine.
 */
export type BuiltinPrompt = "envelope-contract" | "classifier";

/** Directory an operator drops overrides into, relative to the workspace. */
export const PROMPT_DIR = path.join(".omp", "adw", "prompts");

/**
 * Metadata a prompt file may declare above its body.
 *
 * Kept deliberately small. Anything that changes engine behaviour belongs in
 * the workflow YAML, where it is validated; frontmatter here describes the
 * text, it does not configure the run.
 */
export interface PromptFrontmatter {
	/** What this prompt is for, for a reader who found the file first. */
	description?: string;
	/** Free-form provenance: who adapted it, from where, why. */
	source?: string;
}

export interface LoadedPrompt {
	text: string;
	frontmatter: PromptFrontmatter;
	/** Absolute path when an override supplied it; undefined when bundled. */
	path?: string;
}

/**
 * Split `---` frontmatter from the body.
 *
 * Hand-rolled rather than pulled from a YAML parser: the accepted shape is two
 * optional string keys, and accepting arbitrary YAML here would invite
 * structure that nothing reads.
 *
 * @param raw - File contents
 * @returns The body and whatever frontmatter was declared
 */
export function parsePromptFile(raw: string): { text: string; frontmatter: PromptFrontmatter } {
	const match = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(raw);
	if (!match) return { text: raw.trim(), frontmatter: {} };

	const frontmatter: PromptFrontmatter = {};
	for (const line of match[1]?.split(/\r?\n/) ?? []) {
		const entry = /^(description|source):\s*(.*)$/.exec(line.trim());
		if (!entry?.[1]) continue;
		// Strip one layer of quoting; a YAML-quoted value is the common case
		// and leaving the quotes in would put them in the prompt.
		const value = (entry[2] ?? "").trim().replace(/^["'](.*)["']$/, "$1");
		if (value) frontmatter[entry[1] as keyof PromptFrontmatter] = value;
	}
	return { text: raw.slice(match[0].length).trim(), frontmatter };
}

const cache = new Map<string, LoadedPrompt>();

/**
 * Resolve a built-in prompt, preferring an operator's override.
 *
 * An unreadable or empty override falls back to the bundled text and says so
 * once. Failing the run instead would make a typo in an optional file break a
 * workflow that never asked for the override.
 *
 * @param name - Which built-in prompt to resolve
 * @param bundled - The text compiled into this build
 * @param cwd - Workspace root to look under
 * @returns The prompt to use, and where it came from
 */
export function loadPrompt(name: BuiltinPrompt, bundled: string, cwd: string): LoadedPrompt {
	const key = `${cwd}\u0000${name}`;
	const hit = cache.get(key);
	if (hit) return hit;

	const file = path.join(cwd, PROMPT_DIR, `${name}.md`);
	let resolved: LoadedPrompt = { text: bundled, frontmatter: {} };
	try {
		const raw = fs.readFileSync(file, "utf8");
		const parsed = parsePromptFile(raw);
		if (parsed.text) resolved = { ...parsed, path: file };
		else logger.warn("adw prompt override is empty; using the bundled text", { file });
	} catch (error) {
		// ENOENT is the normal case: no override, nothing to report.
		if ((error as NodeJS.ErrnoException)?.code !== "ENOENT") {
			logger.warn("adw could not read a prompt override; using the bundled text", { file, error });
		}
	}
	cache.set(key, resolved);
	return resolved;
}

/** Drop cached overrides. Exported for tests, which write files between cases. */
export function resetPromptCache(): void {
	cache.clear();
}

/**
 * A stable, short identity for prompt text.
 *
 * The trace records which prompt produced an attempt, not the prompt itself:
 * the text can be thousands of characters and is already versioned in git,
 * while what a reader needs from the trace is whether two attempts ran under
 * the same instructions. A digest answers that and costs one string.
 *
 * @param text - Resolved prompt text
 * @returns First 12 hex characters of its SHA-256
 */
export function promptDigest(text: string): string {
	return new Bun.CryptoHasher("sha256").update(text).digest("hex").slice(0, 12);
}

/**
 * How a resolved prompt should be described in a trace.
 *
 * Names the source as well as the digest: a digest alone tells a reader two
 * runs differed without telling them where to look, and `bundled` versus a
 * path is the first question they will ask.
 *
 * @param name - Which built-in prompt
 * @param loaded - The resolution to describe
 * @returns One line naming the origin and digest
 */
export function describePromptOrigin(name: BuiltinPrompt, loaded: LoadedPrompt): string {
	const origin = loaded.path ? path.relative(process.cwd(), loaded.path) : "bundled";
	return `${name}: ${origin} (${promptDigest(loaded.text)})`;
}
