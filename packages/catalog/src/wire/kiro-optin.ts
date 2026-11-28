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

/** What the provider itself says, and what OMP's Kiro provider does, in one sentence each. */
const STATEMENT =
	"Kiro's FAQ (https://kiro.dev/faq) says use of a Kiro subscription through third-party harnesses that route " +
	"requests outside Kiro's native interfaces is not permitted. OMP's Kiro provider calls Kiro's service directly " +
	"and presents the official Kiro CLI's identity. The documented route for other clients is ACP (`kiro-cli acp`).";

/** The notice shown to a user who opted in, at login. */
export const KIRO_OPT_IN_NOTICE = `Kiro is enabled by your opt-in (${KIRO_OPT_IN_ENV}). ${STATEMENT} Continue only if you accept that risk to your Kiro account.`;

/** Raised, with the notice and the way to opt in, when Kiro is used without the opt-in. */
export class KiroNotEnabledError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "KiroNotEnabledError";
	}
}

/** Whether the user opted in. Only `1`, `true` or `yes`, exactly as written (no spaces), in any case. */
export function isKiroOptedIn(env: Record<string, string | undefined> = process.env): boolean {
	const value = env[KIRO_OPT_IN_ENV];
	return value !== undefined && ["1", "true", "yes"].includes(value.toLowerCase());
}

/** Throws a {@link KiroNotEnabledError} unless the user opted in. */
export function assertKiroOptIn(env: Record<string, string | undefined> = process.env): void {
	if (isKiroOptedIn(env)) return;
	throw new KiroNotEnabledError(
		`Kiro is disabled by default. ${STATEMENT} To use this provider anyway, accepting that risk, set ${KIRO_OPT_IN_ENV}=1.`,
	);
}
