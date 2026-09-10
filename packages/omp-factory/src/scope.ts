/** Write-scope verification: the actual change-set must stay inside the declared scope. */

export interface ScopeVerdict {
	ok: boolean;
	violations: string[];
}

/**
 * Match one repo-relative path against one glob. Supports a `**` suffix, a
 * `*` wildcard inside a segment, and exact paths — the shapes the workflow
 * files actually use.
 */
function matchesScopeGlob(pattern: string, changed: string): boolean {
	const scope = pattern.replace(/\/+$/, "");
	if (scope === "**" || scope === "") return true;
	if (scope.endsWith("/**")) {
		const prefix = scope.slice(0, -3);
		if (changed === prefix || changed.startsWith(`${prefix}/`)) return true;
	}
	if (!scope.includes("*")) return changed === scope;
	const expression = scope
		.split("**/")
		.map(part => part.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*"))
		.join("(?:.*/)?");
	return new RegExp(`^${expression}$`).test(changed);
}

/**
 * A change-set is in scope when every path matches an allowed glob and none
 * matches a protected one. Protection wins: a writer authorized for `**` still
 * may not touch the evaluator or the workflow that judges it.
 */
export function verifyScope(
	changedFiles: string[],
	allowedGlobs: string[],
	protectedGlobs: string[] = [],
): ScopeVerdict {
	const violations: string[] = [];
	for (const changed of changedFiles) {
		if (protectedGlobs.some(pattern => matchesScopeGlob(pattern, changed))) {
			violations.push(`${changed} (protected)`);
			continue;
		}
		if (!allowedGlobs.some(pattern => matchesScopeGlob(pattern, changed))) violations.push(changed);
	}
	return { ok: violations.length === 0, violations };
}
