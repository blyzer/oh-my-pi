/**
 * Kiro is off until the user opts in.
 *
 * Kiro's FAQ (https://kiro.dev/faq, "With which tools can I use my Kiro subscription?") lists its own
 * interfaces and ACP-compatible IDEs, and says use through third-party harnesses that route requests
 * outside Kiro's native interfaces is not permitted. OMP's Kiro provider calls Kiro's service directly
 * with the official CLI's identity (`./kiro-cli`), so it stays disabled until the user opts in.
 * Every Kiro request, login and refresh takes its headers from `./kiro-cli`, which asks this module first.
 */

/** Environment variable that turns Kiro on. Only `1`, `true` or `yes` (any case) count. */
export const KIRO_OPT_IN_ENV = "OMP_KIRO_THIRD_PARTY_OPT_IN";

/** The notice shown to a user who opted in, at login. */
export const KIRO_OPT_IN_NOTICE = "not implemented";

/** Raised, with the notice and the way to opt in, when Kiro is used without the opt-in. */
export class KiroNotEnabledError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "KiroNotEnabledError";
	}
}

export function isKiroOptedIn(_env?: Record<string, string | undefined>): boolean {
	throw new Error("not implemented");
}

/** Throws a {@link KiroNotEnabledError} unless the user opted in. */
export function assertKiroOptIn(_env?: Record<string, string | undefined>): void {
	throw new Error("not implemented");
}
