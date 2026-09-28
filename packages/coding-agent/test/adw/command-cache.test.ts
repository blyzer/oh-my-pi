import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, describe, expect, it } from "bun:test";
import { CommandCache, phaseIsCacheable } from "@oh-my-pi/pi-coding-agent/adw/command-cache";

const roots: string[] = [];

function repo(files: Record<string, string>): string {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "adw-cache-"));
	roots.push(root);
	const git = (...args: string[]) => execFileSync("git", args, { cwd: root, stdio: "pipe" });
	git("init", "-q");
	git("config", "user.email", "test@example.com");
	git("config", "user.name", "Test");
	for (const [name, body] of Object.entries(files)) {
		fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
		fs.writeFileSync(path.join(root, name), body);
	}
	git("add", "-A");
	git("commit", "-qm", "seed");
	return root;
}

afterEach(() => {
	for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

describe("command cache", () => {
	it("serves an identical command from the memo and misses once a tracked file changes", async () => {
		const root = repo({ "src/lib.rs": "fn main() {}\n" });
		const cache = new CommandCache(true);

		const first = await cache.lookup("cargo test", root, undefined);
		expect(first.hit).toBe(false);
		expect(first.hit === false && first.reason).toBe("no-entry");
		cache.store(first.hit === false ? first.key : null, { exitCode: 0, summary: "ok", elapsedMs: 1_000 });

		// Same bytes, same command: the verdict cannot differ.
		const second = await cache.lookup("cargo test", root, undefined);
		expect(second.hit).toBe(true);
		expect(second.hit === true && second.run.summary).toBe("ok");
		expect(cache.stats).toEqual({ hits: 1, savedMs: 1_000 });

		// One byte of a tracked file, and the prior verdict no longer applies.
		fs.writeFileSync(path.join(root, "src/lib.rs"), "fn main() { todo!() }\n");
		const third = await cache.lookup("cargo test", root, undefined);
		expect(third.hit).toBe(false);
		expect(third.hit === false && third.reason).toBe("no-entry");
	});

	it("refuses the memo while an untracked file is present", async () => {
		const root = repo({ "src/lib.rs": "fn main() {}\n" });
		const cache = new CommandCache(true);

		const primed = await cache.lookup("cargo test", root, undefined);
		cache.store(primed.hit === false ? primed.key : null, { exitCode: 0, summary: "ok", elapsedMs: 10 });
		expect((await cache.lookup("cargo test", root, undefined)).hit).toBe(true);

		// An untracked file is content the fingerprint cannot see. Serving a
		// verdict now would replay a result that never observed these bytes.
		fs.writeFileSync(path.join(root, "scratch.rs"), "fn extra() {}\n");
		const dirty = await cache.lookup("cargo test", root, undefined);
		expect(dirty.hit).toBe(false);
		expect(dirty.hit === false && dirty.reason).toBe("dirty-untracked");
	});

	it("keys on the command and on the caller-supplied environment", async () => {
		const root = repo({ "src/lib.rs": "fn main() {}\n" });
		const cache = new CommandCache(true);

		const primed = await cache.lookup("cargo test", root, { PROFILE: "dev" });
		cache.store(primed.hit === false ? primed.key : null, { exitCode: 0, summary: "dev ok", elapsedMs: 5 });

		expect((await cache.lookup("cargo test --release", root, { PROFILE: "dev" })).hit).toBe(false);
		expect((await cache.lookup("cargo test", root, { PROFILE: "release" })).hit).toBe(false);
		expect((await cache.lookup("cargo test", root, { PROFILE: "dev" })).hit).toBe(true);
	});

	it("refuses anything outside a repository rather than guessing", async () => {
		const bare = fs.mkdtempSync(path.join(os.tmpdir(), "adw-cache-bare-"));
		roots.push(bare);
		const cache = new CommandCache(true);
		const miss = await cache.lookup("cargo test", bare, undefined);
		expect(miss.hit).toBe(false);
		expect(miss.hit === false && miss.reason).toBe("not-a-repo");
		expect(miss.hit === false && miss.key).toBeNull();
	});

	it("stores nothing when disabled", async () => {
		const root = repo({ "src/lib.rs": "fn main() {}\n" });
		const cache = new CommandCache(false);
		const miss = await cache.lookup("cargo test", root, undefined);
		expect(miss.hit).toBe(false);
		expect(miss.hit === false && miss.reason).toBe("disabled");
		// A null key makes storing a no-op, so a disabled cache cannot be
		// primed by a caller that ignores the reason.
		cache.store(miss.hit === false ? miss.key : null, { exitCode: 0, summary: "ok", elapsedMs: 1 });
		expect((await cache.lookup("cargo test", root, undefined)).hit).toBe(false);
	});

	it("never memoizes an expect: fail phase", () => {
		// The inverted criterion exists to prove a test genuinely runs and
		// genuinely fails. A memo would satisfy it without executing anything.
		expect(phaseIsCacheable({ cache: true, expect: "fail" })).toBe(false);
		expect(phaseIsCacheable({ cache: true, expect: "pass" })).toBe(true);
		expect(phaseIsCacheable({ cache: true })).toBe(true);
		// Opt-in: silence means no.
		expect(phaseIsCacheable({})).toBe(false);
		expect(phaseIsCacheable({ cache: false })).toBe(false);
	});
});
