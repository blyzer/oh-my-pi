/**
 * Admission is a throughput guard, not a safety boundary, so its contract is
 * narrow and worth pinning exactly: it must be able to refuse, it must never
 * refuse the only candidate, and it must not stall forever on a host that
 * stays saturated.
 */
import { describe, expect, it } from "bun:test";
import { hostCapacity } from "@oh-my-pi/pi-natives";
import {
	type AdmissionPolicy,
	awaitAdmission,
	classifyFailure,
	DEFAULT_ADMISSION_POLICY,
	judgeAdmission,
} from "../../src/task/admission";

/** Nothing on a real host can satisfy this. */
const IMPOSSIBLE: AdmissionPolicy = {
	minAvailableMemoryRatio: 1.01,
	minAvailableDiskBytes: Number.MAX_SAFE_INTEGER,
	maxLoadPerCpu: 0,
	retryAfterMs: 5,
};

describe("host capacity", () => {
	it("reports metrics this platform can actually answer", () => {
		const capacity = hostCapacity(process.cwd());
		// Total RAM and CPU count are answerable everywhere the agent runs.
		expect(capacity.totalMemory).toBeGreaterThan(0);
		expect(capacity.cpus).toBeGreaterThan(0);
		// The optional metrics are `undefined` when unavailable, never a
		// fabricated zero -- a caller must be able to tell "no capacity" from
		// "no answer".
		for (const value of [capacity.availableMemory, capacity.availableDisk, capacity.loadAverage]) {
			if (value !== undefined) expect(value).toBeGreaterThan(0);
		}
	});
});

describe("judgeAdmission", () => {
	it("admits the only candidate however loaded the host is", () => {
		// Refusing here frees nothing: no running holder can release capacity.
		expect(judgeAdmission(process.cwd(), 0, IMPOSSIBLE)).toEqual({ admit: true });
	});

	it("refuses an additional start when the host cannot afford it", () => {
		const verdict = judgeAdmission(process.cwd(), 1, IMPOSSIBLE);
		expect(verdict.admit).toBeFalse();
		if (verdict.admit) return;
		expect(verdict.reason.length).toBeGreaterThan(0);
		expect(verdict.retryAfterMs).toBeGreaterThan(0);
	});

	it("keeps the shipped ceilings above what an ordinary host reports", () => {
		// Asserting "this machine admits" would pin the suite to the load of
		// whatever runs it -- measured at 79 on 10 CPUs here while the host
		// stayed responsive. Pin the policy instead: the floors must stay
		// permissive enough that a busy developer laptop is not serialized.
		expect(DEFAULT_ADMISSION_POLICY.minAvailableMemoryRatio).toBeLessThanOrEqual(0.15);
		expect(DEFAULT_ADMISSION_POLICY.maxLoadPerCpu).toBeGreaterThanOrEqual(16);
		// Memory pressure is the kernel's own verdict and always refuses, so
		// the ratio floor must not be the only thing standing between a
		// saturated host and a pile-on.
		expect(DEFAULT_ADMISSION_POLICY.retryAfterMs).toBeGreaterThan(0);
	});
});

describe("awaitAdmission", () => {
	it("gives up waiting rather than stalling a saturated host forever", async () => {
		const started = Date.now();
		const verdict = await awaitAdmission({
			workspace: process.cwd(),
			inFlight: () => 1,
			policy: IMPOSSIBLE,
			maxWaitMs: 60,
		});
		// Returning `admit: false` is the point: the caller proceeded anyway,
		// and the verdict says so instead of pretending capacity appeared.
		expect(verdict.admit).toBeFalse();
		expect(Date.now() - started).toBeLessThan(5_000);
	});

	it("returns immediately once capacity is available", async () => {
		let inFlight = 1;
		const verdict = await awaitAdmission({
			workspace: process.cwd(),
			inFlight: () => {
				const current = inFlight;
				inFlight = 0; // a holder released between samples
				return current;
			},
			policy: IMPOSSIBLE,
			maxWaitMs: 5_000,
		});
		expect(verdict.admit).toBeTrue();
	});
});

describe("classifyFailure", () => {
	it("separates host failures from failures an agent could fix", () => {
		// Spending a correction attempt on an OOM asks an agent to fix code
		// that never ran -- the specific waste this exists to prevent.
		expect(classifyFailure("fatal error: out of memory")).toBe("resource");
		expect(classifyFailure("ENOSPC: no space left on device")).toBe("resource");
		expect(classifyFailure("expected 3 to equal 4")).toBe("semantic");
		expect(classifyFailure("TypeError: x is not a function")).toBe("semantic");
	});

	it("defers to the provider's own classification when given the error", () => {
		// The provider layer already knows a 429 from a bad tool schema, so
		// this must not re-derive it from message text and drift apart.
		expect(classifyFailure("request failed", { status: 429 })).toBe("resource");
		expect(classifyFailure("request failed", { status: 503 })).toBe("resource");
		expect(classifyFailure("request failed", { status: 400 })).toBe("semantic");
	});
});
