/**
 * Admission control: logical readiness is not physical admissibility.
 *
 * A semaphore answers "are fewer than N running?". It cannot answer "can this
 * machine afford one more?". Those differ exactly when it matters: an
 * isolated writer copies a tree, a write guard snapshots one, and several of
 * those starting together on a loaded host degrade every one of them rather
 * than finishing any sooner.
 *
 * This gate sits IN FRONT of the concurrency semaphore, never replacing it.
 * The semaphore bounds the steady state; admission delays the next start
 * while the host is already saturated. One admitted worker always proceeds —
 * a gate that can starve the last runnable task converts backpressure into a
 * deadlock.
 */
import * as AIError from "@oh-my-pi/pi-ai/error";
import { hostCapacity } from "@oh-my-pi/pi-natives";

/** Why a start was delayed, for logs and for failure classification. */
export type AdmissionVerdict = { admit: true } | { admit: false; reason: string; retryAfterMs: number };

export interface AdmissionPolicy {
	/** Minimum available memory to start new work, as a fraction of RAM. */
	minAvailableMemoryRatio: number;
	/** Minimum free bytes on the workspace filesystem. */
	minAvailableDiskBytes: number;
	/** Maximum 1-minute load average per CPU before new starts wait. */
	maxLoadPerCpu: number;
	/** How long to wait before re-sampling when a start is refused. */
	retryAfterMs: number;
}

/**
 * Deliberately permissive. Admission exists to stop a pile-on, not to
 * second-guess a healthy machine: the memory floor sits just above the
 * kernel's own 15% pressure threshold.
 *
 * The load ceiling is high because load average is not comparable across
 * platforms. Linux counts uninterruptible-sleep tasks, so a host blocked on
 * I/O reports a large number while its CPUs idle; macOS reports figures well
 * above `cpus` under ordinary interactive use — measured here at 79 on 10
 * CPUs while the machine stayed responsive. A ceiling tuned to the textbook
 * "1.0 per CPU is saturated" would refuse nearly every start on a developer
 * laptop, which is why memory and disk carry the real signal and load only
 * catches the extreme.
 */
export const DEFAULT_ADMISSION_POLICY: AdmissionPolicy = {
	minAvailableMemoryRatio: 0.15,
	minAvailableDiskBytes: 2 * 1024 ** 3,
	maxLoadPerCpu: 24,
	retryAfterMs: 2_000,
};

/**
 * Judge one prospective start against current host capacity.
 *
 * `inFlight === 0` always admits: whatever the host looks like, refusing the
 * only candidate makes no progress and frees nothing.
 *
 * A metric the platform does not report is absent (napi maps `Option<T>` to
 * `undefined`, not `null`), and an absent metric never
 * refuses. That is not fail-open sloppiness — the check is a throughput
 * guard, not a safety boundary, and a Windows host reporting no load average
 * must not stall forever. Safety boundaries (scope, gates, acceptance) fail
 * closed elsewhere.
 */
export function judgeAdmission(
	workspace: string,
	inFlight: number,
	policy: AdmissionPolicy = DEFAULT_ADMISSION_POLICY,
): AdmissionVerdict {
	if (inFlight === 0) return { admit: true };

	const capacity = hostCapacity(workspace);
	const { retryAfterMs } = policy;

	if (capacity.underMemoryPressure) {
		return { admit: false, reason: "host is under memory pressure", retryAfterMs };
	}

	if (capacity.availableMemory !== undefined && capacity.totalMemory > 0) {
		const ratio = capacity.availableMemory / capacity.totalMemory;
		if (ratio < policy.minAvailableMemoryRatio) {
			const percent = (ratio * 100).toFixed(0);
			return { admit: false, reason: `available memory ${percent}% below floor`, retryAfterMs };
		}
	}

	if (capacity.availableDisk !== undefined && capacity.availableDisk < policy.minAvailableDiskBytes) {
		const gb = (capacity.availableDisk / 1024 ** 3).toFixed(1);
		return { admit: false, reason: `workspace filesystem has ${gb}GB free`, retryAfterMs };
	}

	if (capacity.loadAverage !== undefined && capacity.cpus > 0) {
		const perCpu = capacity.loadAverage / capacity.cpus;
		if (perCpu > policy.maxLoadPerCpu) {
			return {
				admit: false,
				reason: `load ${capacity.loadAverage.toFixed(1)} across ${capacity.cpus} cpus`,
				retryAfterMs,
			};
		}
	}

	return { admit: true };
}

/**
 * Hold a start until the host can afford it.
 *
 * Called AFTER the concurrency permit is held, so the wait is bounded by the
 * permit itself and a refused start keeps its place in line rather than
 * losing it to a later arrival. `maxWaitMs` caps the delay: a host that
 * stays saturated must eventually make progress, since the alternative is a
 * queue that never drains and work that never fails either.
 *
 * Returns the verdict that ended the wait — `admit: false` means the cap
 * elapsed and the caller proceeded anyway, which is worth logging.
 */
export async function awaitAdmission(options: {
	workspace: string;
	inFlight: () => number;
	signal?: AbortSignal;
	maxWaitMs?: number;
	policy?: AdmissionPolicy;
	onWait?: (reason: string) => void;
}): Promise<AdmissionVerdict> {
	const { workspace, inFlight, signal, maxWaitMs = 60_000, policy, onWait } = options;
	const deadline = Date.now() + maxWaitMs;
	let notified = false;
	for (;;) {
		const verdict = judgeAdmission(workspace, inFlight(), policy);
		if (verdict.admit || signal?.aborted || Date.now() >= deadline) return verdict;
		if (!notified) {
			notified = true;
			onWait?.(verdict.reason);
		}
		const remaining = Math.max(0, deadline - Date.now());
		await Bun.sleep(Math.min(verdict.retryAfterMs, remaining));
	}
}

/**
 * Failure attribution. A run killed by the host did not fail semantically,
 * and spending a correction attempt asking an agent to fix code that never
 * ran is the specific waste this classification prevents.
 */
export type FailureClass = "semantic" | "resource";

const RESOURCE_SIGNATURES = [
	/\bENOMEM\b/i,
	/\bENOSPC\b/i,
	/out of memory/i,
	/cannot allocate memory/i,
	/no space left on device/i,
	/killed\b.*\bsignal 9\b/i,
	/\bSIGKILL\b/,
	/too many open files/i,
	/\bEMFILE\b/i,
	/resource temporarily unavailable/i,
	/\bEAGAIN\b/i,
];

/**
 * Classify a failure as resource-caused or semantic.
 *
 * Two sources, deliberately: the provider layer already classifies its own
 * errors (rate limit, quota, transport), so an `error` object is asked
 * there first rather than re-derived from its text here. Free text — a
 * killed process, a build log — falls back to signatures the provider layer
 * never sees.
 *
 * Fails toward `semantic`: a misread resource failure wastes an attempt,
 * while a misread semantic failure would silently skip the correction the
 * run exists to perform.
 */
export function classifyFailure(text: string, error?: unknown): FailureClass {
	if (error !== undefined) {
		const id = AIError.classify(error);
		if (AIError.is(id, AIError.Flag.UsageLimit) || AIError.is(id, AIError.Flag.Transient)) {
			return "resource";
		}
		const httpStatus = AIError.status(error);
		if (httpStatus === 429 || httpStatus === 503 || httpStatus === 529) return "resource";
	}
	return RESOURCE_SIGNATURES.some(pattern => pattern.test(text)) ? "resource" : "semantic";
}
