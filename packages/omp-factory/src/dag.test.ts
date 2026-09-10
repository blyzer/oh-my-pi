import { describe, expect, it } from "bun:test";
import { buildWaves, readyPhases, VersionStore } from "./dag";

describe("buildWaves", () => {
	it("schedules a diamond in deterministic waves", () => {
		const deps = {
			architect: [],
			backend: ["architect"],
			frontend: ["architect"],
			reviewer: ["backend", "frontend"],
		};
		expect(buildWaves(Object.keys(deps), deps)).toEqual([["architect"], ["backend", "frontend"], ["reviewer"]]);
	});

	it("rejects cycles instead of guessing an order", () => {
		const deps = { a: ["b"], b: ["a"] };
		expect(() => buildWaves(Object.keys(deps), deps)).toThrow("dependency cycle");
	});
});

describe("readyPhases", () => {
	const deps = { architect: [], backend: ["architect"], frontend: ["architect"], reviewer: ["backend", "frontend"] };

	it("unlocks only phases whose dependencies are accepted", () => {
		expect(readyPhases(deps, new Set(), new Set())).toEqual(["architect"]);
		expect(readyPhases(deps, new Set(["architect"]), new Set(["architect"]))).toEqual(["backend", "frontend"]);
	});

	it("keeps the reviewer blocked when backend failed permanently", () => {
		const accepted = new Set(["architect", "frontend"]);
		const decided = new Set(["architect", "frontend", "backend"]);
		expect(readyPhases(deps, accepted, decided)).toEqual([]);
	});

	it("never readies an already-decided phase", () => {
		expect(readyPhases(deps, new Set(["architect"]), new Set(["architect", "backend"]))).toEqual(["frontend"]);
	});
});

describe("VersionStore", () => {
	it("pins the consumed version against later producer corrections", () => {
		const store = new VersionStore();
		store.accept({ phase: "plan", version: 1, digest: "d1", artifacts: ["plan.md"] });
		const consumed = store.select("plan", 1);
		store.accept({ phase: "plan", version: 2, digest: "d2", artifacts: ["plan.md"] });
		expect(consumed).toMatchObject({ version: 1, digest: "d1" });
		expect(store.select("plan")).toMatchObject({ version: 2, digest: "d2" });
	});

	it("returns null when nothing was accepted", () => {
		expect(new VersionStore().select("plan")).toBeNull();
	});

	it("rejects version gaps instead of silently reordering history", () => {
		const store = new VersionStore();
		expect(() => store.accept({ phase: "plan", version: 2, digest: "d2", artifacts: [] })).toThrow("version gap");
	});
});
