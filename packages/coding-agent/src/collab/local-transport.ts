/**
 * A {@link CollabTransport} that carries collab frames over a unix socket on
 * this machine, so a local supervisor can attach to a live TUI session without
 * the session's traffic leaving the host.
 *
 * Three deliberate differences from {@link CollabSocket}, each because the
 * relay's problem is not this transport's problem:
 *
 * - **No AES-GCM.** The room key exists to keep a relay operator out of the
 *   plaintext. There is no operator here; the socket's 0600 mode and its
 *   location under the per-user runtime root are the boundary, and adding a key
 *   would only move the question to where the key is kept.
 * - **No reconnect.** The relay client retries because the network is between
 *   the peers. Here the peer is a process on the same machine: a closed
 *   connection means it exited, and pretending otherwise would keep a dead
 *   guest's peer id alive in the host's table.
 * - **Frames are newline-delimited JSON.** The relay's binary envelope carries
 *   a peer id because a relay fans out to many guests; this server assigns its
 *   own ids on accept and knows which connection a frame came from.
 *
 * The capability split survives all three: the host still mints a write token
 * and still verifies it in `#verifyWriteToken`, so a guest that connects to the
 * socket without the token is read-only exactly as a relay guest would be.
 */
import * as fs from "node:fs/promises";
import * as net from "node:net";
import * as path from "node:path";
import { logger } from "@oh-my-pi/pi-utils";
import type { RelayControlMessage } from "@oh-my-pi/pi-wire";
import type { CollabFrame } from "./protocol";
import type { CollabTransport } from "./transport";

/** Guard against a peer streaming an unbounded line into memory. */
const MAX_LINE_BYTES = 8 * 1024 * 1024;

export interface CollabLocalServerOptions {
	/** Absolute path for the listening socket. Parent directory is created 0700. */
	socketPath: string;
}

export class CollabLocalServer implements CollabTransport {
	onOpen?: () => void;
	onFrame?: (frame: CollabFrame, fromPeer: number) => void;
	onControl?: (msg: RelayControlMessage) => void;
	onClose?: (reason: string, willReconnect: boolean) => void;

	readonly #socketPath: string;
	#server: net.Server | null = null;
	#peers = new Map<number, net.Socket>();
	#buffers = new Map<number, string>();
	#nextPeer = 1;
	#closed = false;

	constructor(opts: CollabLocalServerOptions) {
		this.#socketPath = opts.socketPath;
	}

	get socketPath(): string {
		return this.#socketPath;
	}

	/**
	 * `connect()` on a server means "listen". The contract only promises that
	 * `onOpen` fires once the transport is usable, and a listening socket with
	 * no guest yet is usable — the host's welcome is sent per-peer on hello,
	 * not on open.
	 */
	connect(): void {
		if (this.#server || this.#closed) return;
		void this.#listen();
	}

	async #listen(): Promise<void> {
		try {
			await fs.mkdir(path.dirname(this.#socketPath), { recursive: true, mode: 0o700 });
			await fs.rm(this.#socketPath, { force: true });
			const server = net.createServer(socket => this.#accept(socket));
			this.#server = server;
			const { promise: listening, resolve, reject } = Promise.withResolvers<void>();
			server.once("listening", resolve);
			server.once("error", reject);
			server.listen(this.#socketPath);
			await listening;
			await fs.chmod(this.#socketPath, 0o600);
			server.on("error", err => {
				logger.debug("collab local: server error", { error: String(err) });
			});
			this.onOpen?.();
		} catch (err) {
			this.#closed = true;
			this.onClose?.(err instanceof Error ? err.message : String(err), false);
		}
	}

	#accept(socket: net.Socket): void {
		const peer = this.#nextPeer++;
		this.#peers.set(peer, socket);
		this.#buffers.set(peer, "");
		socket.setNoDelay(true);
		socket.on("data", (chunk: Buffer) => this.#ingest(peer, chunk));
		socket.on("error", err => {
			logger.debug("collab local: peer error", { peer, error: String(err) });
		});
		socket.once("close", () => this.#dropPeer(peer));
	}

	#ingest(peer: number, chunk: Buffer): void {
		let buffered = (this.#buffers.get(peer) ?? "") + chunk.toString("utf8");
		for (;;) {
			const newline = buffered.indexOf("\n");
			if (newline === -1) break;
			const line = buffered.slice(0, newline);
			buffered = buffered.slice(newline + 1);
			if (line.length === 0) continue;
			let frame: CollabFrame;
			try {
				frame = JSON.parse(line) as CollabFrame;
			} catch {
				// A peer that cannot produce valid JSON cannot be reasoned with;
				// dropping the line beats tearing down a session over it.
				logger.debug("collab local: unparseable frame", { peer, bytes: line.length });
				continue;
			}
			this.onFrame?.(frame, peer);
		}
		if (buffered.length > MAX_LINE_BYTES) {
			logger.debug("collab local: peer line overflow, dropping", { peer, bytes: buffered.length });
			this.#peers.get(peer)?.destroy();
			return;
		}
		this.#buffers.set(peer, buffered);
	}

	#dropPeer(peer: number): void {
		if (!this.#peers.delete(peer)) return;
		this.#buffers.delete(peer);
		// The host prunes its own peer table from this, exactly as it does for a
		// relay `peer-left`; without it a departed guest keeps a live entry and
		// every broadcast writes to a dead socket.
		this.onControl?.({ t: "peer-left", peer });
	}

	send(frame: CollabFrame, targetPeer = 0): void {
		if (this.#closed) return;
		const line = `${JSON.stringify(frame)}\n`;
		if (targetPeer === 0) {
			for (const socket of this.#peers.values()) socket.write(line);
			return;
		}
		this.#peers.get(targetPeer)?.write(line);
	}

	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const socket of this.#peers.values()) socket.destroy();
		this.#peers.clear();
		this.#buffers.clear();
		const server = this.#server;
		this.#server = null;
		if (server) {
			server.close();
			void fs.rm(this.#socketPath, { force: true }).catch(() => {});
		}
	}
}
