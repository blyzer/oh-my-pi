/**
 * Runs in flight, so a workflow can be stopped. One entry per run: a workflow
 * can spend an hour across several models, and `/adw` commands cannot reach
 * the TUI's Esc path on their own — {@link cancelActiveAdwRuns} is what the
 * input controller calls.
 *
 * A leaf module on purpose: the input controller imports it, and reaching it
 * through the `/adw` command module pulled the whole runner graph — and with
 * it the builtin slash-command registry — into the controller's import cycle.
 */
const active = new Map<string, { controller: AbortController; workflow: string }>();

/** Records a run so {@link cancelActiveAdwRuns} can abort it. */
export function trackAdwRun(key: string, controller: AbortController, workflow: string): void {
	active.set(key, { controller, workflow });
}

/** Forgets a run once it has settled. */
export function untrackAdwRun(key: string): void {
	active.delete(key);
}

export function hasActiveAdwRun(): boolean {
	return active.size > 0;
}

/** Aborts every in-flight workflow. Returns false when there was nothing to stop. */
export function cancelActiveAdwRuns(): boolean {
	if (active.size === 0) return false;
	for (const [key, entry] of active) {
		entry.controller.abort(new Error(`${entry.workflow} interrupted`));
		active.delete(key);
	}
	return true;
}
