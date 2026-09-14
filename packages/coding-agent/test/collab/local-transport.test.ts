/**
 * End-to-end contract for attaching to a live session over a local socket:
 * `CollabHost.startLocal` shares the SAME session the TUI is driving, a guest
 * that presents the write token may prompt it, and a guest that does not is
 * refused — the capability split that makes the relay path safe is the same
 * code on the local path, so it must behave identically.
 *
 * Runs over a real unix socket with a real `CollabLocalServer`; only the
 * `InteractiveModeContext` is a double, so the handshake, the peer table, the
 * write-token check and the prompt/abort routing are all exercised for real.
 */
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, describe, expect, it } from "bun:test";
import { CollabHost } from "@oh-my-pi/pi-coding-agent/collab/host";
import { CollabLocalServer } from "@oh-my-pi/pi-coding-agent/collab/local-transport";
import { COLLAB_PROTO, type CollabFrame } from "@oh-my-pi/pi-coding-agent/collab/protocol";
import type { InteractiveModeContext } from "@oh-my-pi/pi-coding-agent/modes/types";
import type { AgentSession } from "@oh-my-pi/pi-coding-agent/session/agent-session";

interface HostHarness {
	ctx: InteractiveModeContext;
	prompts: { from?: string }[];
	aborts: { count: number };
	/** Identity of the session object the host was handed, to prove it is not replaced. */
	sessionIdentity: object;
	/** Resolves when the host next routes a prompt into the session. */
	nextPrompt(): Promise<void>;
	/** Resolves when the host next routes an abort into the session. */
	nextAbort(): Promise<void>;
}

function makeHostContext(): HostHarness {
	const prompts: { from?: string }[] = [];
	const aborts = { count: 0 };
	const promptWaiters: (() => void)[] = [];
	const abortWaiters: (() => void)[] = [];
	const session = {
		isStreaming: false,
		queuedMessageCount: 0,
		sessionName: "test",
		model: undefined,
		thinkingLevel: undefined,
		subscribe: () => () => {},
		emitNotice: () => {},
		promptCustomMessage: (message: { details?: { from?: string } }) => {
			prompts.push(message.details ?? {});
			for (const waiter of promptWaiters.splice(0)) waiter();
			return Promise.resolve();
		},
		abort: () => {
			aborts.count++;
			for (const waiter of abortWaiters.splice(0)) waiter();
			return Promise.resolve();
		},
	};
	const ctx = {
		settings: { get: () => "" },
		sessionManager: {
			getSessionId: () => "sess-local",
			getCwd: () => "/tmp",
			snapshotForReplication: () => ({
				header: { type: "session", id: "sess-local", timestamp: new Date().toISOString(), cwd: "/tmp" },
				entries: [],
			}),
			onEntryAppended: undefined,
		},
		session,
		eventBus: undefined,
		statusLine: {
			setCollabStatus: () => {},
			invalidate: () => {},
			getCachedContextBreakdown: () => ({ usedTokens: 0, contextWindow: 0 }),
		},
		ui: { requestRender: () => {} },
		showStatus: () => {},
		collabHost: undefined,
	} as unknown as InteractiveModeContext;
	const nextPrompt = (): Promise<void> => {
		const { promise, resolve } = Promise.withResolvers<void>();
		promptWaiters.push(resolve);
		return promise;
	};
	const nextAbort = (): Promise<void> => {
		const { promise, resolve } = Promise.withResolvers<void>();
		abortWaiters.push(resolve);
		return promise;
	};
	return { ctx, prompts, aborts, sessionIdentity: session, nextPrompt, nextAbort };
}

/** Raw guest speaking newline-delimited frames straight at the socket. */
class LocalGuest {
	readonly #socket: net.Socket;
	#buffer = "";
	readonly #queue: CollabFrame[] = [];
	readonly #waiters: ((frame: CollabFrame) => void)[] = [];

	constructor(socket: net.Socket) {
		this.#socket = socket;
		socket.on("data", (chunk: Buffer) => {
			this.#buffer += chunk.toString("utf8");
			for (;;) {
				const newline = this.#buffer.indexOf("\n");
				if (newline === -1) break;
				const line = this.#buffer.slice(0, newline);
				this.#buffer = this.#buffer.slice(newline + 1);
				if (line.length === 0) continue;
				const frame = JSON.parse(line) as CollabFrame;
				// Debounced broadcasts interleave with directed replies; the
				// assertions here are about welcome and error only.
				if (frame.t !== "welcome" && frame.t !== "error") continue;
				const waiter = this.#waiters.shift();
				if (waiter) waiter(frame);
				else this.#queue.push(frame);
			}
		});
	}

	static async connect(socketPath: string): Promise<LocalGuest> {
		const socket = net.createConnection(socketPath);
		const { promise, resolve } = Promise.withResolvers<void>();
		socket.once("connect", resolve);
		await promise;
		return new LocalGuest(socket);
	}

	send(frame: CollabFrame): void {
		this.#socket.write(`${JSON.stringify(frame)}\n`);
	}

	nextFrame(): Promise<CollabFrame> {
		const queued = this.#queue.shift();
		if (queued) return Promise.resolve(queued);
		const { promise, resolve } = Promise.withResolvers<CollabFrame>();
		this.#waiters.push(resolve);
		return promise;
	}

	close(): void {
		this.#socket.destroy();
	}
}

describe("collab over a local transport", () => {
	const cleanups: (() => void)[] = [];

	afterEach(() => {
		for (const cleanup of cleanups.splice(0)) cleanup();
	});

	async function startHost(): Promise<{
		harness: HostHarness;
		host: CollabHost;
		writeToken: string;
		socketPath: string;
	}> {
		const socketPath = path.join(
			await Bun.file(os.tmpdir()).exists() ? os.tmpdir() : "/tmp",
			`collab-test-${process.pid}-${Math.random().toString(36).slice(2)}`,
			"collab.sock",
		);
		const harness = makeHostContext();
		const host = new CollabHost(harness.ctx);
		const transport = new CollabLocalServer({ socketPath });
		const rawToken = await host.startLocal(transport);
		cleanups.push(() => void host.stop("test over"));
		return { harness, host, writeToken: Buffer.from(rawToken).toString("base64url"), socketPath };
	}

	it("drives the host's existing session, and never replaces it", async () => {
		const { harness, writeToken, socketPath } = await startHost();
		const guest = await LocalGuest.connect(socketPath);
		cleanups.push(() => guest.close());

		guest.send({ t: "hello", name: "bridge", proto: COLLAB_PROTO, writeToken });
		const welcome = await guest.nextFrame();
		expect(welcome.t).toBe("welcome");
		expect((welcome as { readOnly?: true }).readOnly).toBeUndefined();

		const prompted = harness.nextPrompt();
		guest.send({ t: "prompt", text: "from the bridge" });
		await prompted;

		expect(harness.prompts.length).toBe(1);
		// The identity check is the point of the whole design: the prompt landed
		// on the very object the context was constructed with, not on a copy and
		// not on a session the host made for itself.
		expect(harness.ctx.session as unknown as object).toBe(harness.sessionIdentity);

		const aborted = harness.nextAbort();
		guest.send({ t: "abort" });
		await aborted;
		expect(harness.aborts.count).toBe(1);
	});

	it("refuses a guest that presents no write token", async () => {
		const { harness, socketPath } = await startHost();
		const guest = await LocalGuest.connect(socketPath);
		cleanups.push(() => guest.close());

		guest.send({ t: "hello", name: "viewer", proto: COLLAB_PROTO });
		const welcome = await guest.nextFrame();
		expect(welcome.t).toBe("welcome");
		expect((welcome as { readOnly?: true }).readOnly).toBe(true);

		guest.send({ t: "prompt", text: "should not land" });
		const refusal = await guest.nextFrame();
		expect(refusal.t).toBe("error");
		expect(harness.prompts.length).toBe(0);
	});

	it("refuses a forged write token", async () => {
		const { harness, socketPath } = await startHost();
		const guest = await LocalGuest.connect(socketPath);
		cleanups.push(() => guest.close());

		const forged = Buffer.from(new Uint8Array(32).fill(7)).toString("base64url");
		guest.send({ t: "hello", name: "forger", proto: COLLAB_PROTO, writeToken: forged });
		const welcome = await guest.nextFrame();
		expect((welcome as { readOnly?: true }).readOnly).toBe(true);

		guest.send({ t: "prompt", text: "should not land" });
		await guest.nextFrame();
		expect(harness.prompts.length).toBe(0);
	});
});
