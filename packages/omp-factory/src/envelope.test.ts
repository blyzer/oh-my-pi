import { describe, expect, it } from "bun:test";
import { parseEnvelope } from "./envelope";

const good = { status: "success", summary: "did the thing", artifacts: ["src/a.ts"] };

describe("parseEnvelope", () => {
	it("takes the last object after prose and fences", () => {
		const turn = [
			"Here is my reasoning.",
			"```json",
			JSON.stringify({ status: "fail", summary: "draft", artifacts: [] }),
			"```",
			"Final answer:",
			JSON.stringify(good),
		].join("\n");
		const result = parseEnvelope(turn);
		expect(result.ok).toBeTrue();
		if (result.ok) expect(result.envelope).toMatchObject({ status: "success", artifacts: ["src/a.ts"] });
	});

	it("keeps braces inside strings from splitting the span", () => {
		const turn = JSON.stringify({ ...good, summary: "handled } and { in prose" });
		const result = parseEnvelope(turn);
		expect(result.ok).toBeTrue();
		if (result.ok) expect(result.envelope.summary).toBe("handled } and { in prose");
	});

	it("keeps a nested object as one span", () => {
		const turn = `noise ${JSON.stringify({ ...good, extra: { nested: { deep: true } } })}`;
		const result = parseEnvelope(turn);
		expect(result.ok).toBeTrue();
		if (result.ok) expect(result.envelope.artifacts).toEqual(["src/a.ts"]);
	});

	it("rejects prose-only output", () => {
		const result = parseEnvelope("I finished the work, trust me.");
		expect(result.ok).toBeFalse();
		if (!result.ok) expect(result.violation).toContain("no top-level JSON object");
	});

	it("does not treat an unterminated object as a candidate", () => {
		const result = parseEnvelope(`${JSON.stringify(good).slice(0, -1)}`);
		expect(result.ok).toBeFalse();
		if (!result.ok) expect(result.violation).toContain("no top-level JSON object");
	});

	it("rejects an unknown status", () => {
		const result = parseEnvelope(JSON.stringify({ ...good, status: "mostly-done" }));
		expect(result.ok).toBeFalse();
		if (!result.ok) expect(result.violation).toContain("unknown status");
	});

	it("rejects a missing summary", () => {
		const result = parseEnvelope(JSON.stringify({ status: "success", artifacts: [] }));
		expect(result.ok).toBeFalse();
		if (!result.ok) expect(result.violation).toContain("summary");
	});

	it("rejects non-string artifact entries", () => {
		const result = parseEnvelope(JSON.stringify({ ...good, artifacts: ["ok", 7] }));
		expect(result.ok).toBeFalse();
		if (!result.ok) expect(result.violation).toContain("artifacts must be an array of strings");
	});

	it("treats a self-reported failure as a parseable answer, not a parse error", () => {
		const result = parseEnvelope(JSON.stringify({ status: "fail", summary: "could not build", artifacts: [] }));
		expect(result.ok).toBeTrue();
		if (result.ok) expect(result.envelope.status).toBe("fail");
	});

	it("defaults absent artifacts to an empty declaration", () => {
		const result = parseEnvelope(JSON.stringify({ status: "success", summary: "nothing written" }));
		expect(result.ok).toBeTrue();
		if (result.ok) expect(result.envelope.artifacts).toEqual([]);
	});
});
