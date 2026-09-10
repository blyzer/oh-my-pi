/** Write-scope verification: the actual change-set must stay inside the declared scope. */

export interface ScopeVerdict {
	ok: boolean;
	violations: string[];
}

/** Match one repo-relative path against one allowed glob (`**` suffix or exact path). */
function matchesScopeGlob(allowed: string, changed: string): boolean {
	const scope = allowed.replace(/\/+$/, "");
	if (scope === "**" || scope === "") return true;
	if (scope.endsWith("/**")) {
		const prefix = scope.slice(0, -3);
		return changed === prefix || changed.startsWith(`${prefix}/`);
	}
	return changed === scope;
}

export function verifyScope(changedFiles: string[], allowedGlobs: string[]): ScopeVerdict {
	const violations = changedFiles.filter(changed => !allowedGlobs.some(scope => matchesScopeGlob(scope, changed)));
	return { ok: violations.length === 0, violations };
}
