// Fake omp-tui host for `tui.test.ts`. It serves the slice of the
// `OMP_TUI_DEBUG` wire the `tui` tool's `start`/`stop` ops touch and reads
// raw Ctrl-C bytes from its PTY, then quits the way argv selects:
//
//   fake-host <repeat|first|never|quit-op> [no-socket]
//
// `repeat` mirrors `omp chat` (`omp_chat::ctrl_c_action`): only a second
// `C-c` within 500 ms quits. `first` mirrors the examples, which quit on one
// `C-c`. `never` ignores `C-c`, like a chat host behind a modal overlay.
// `quit-op` ignores `C-c` too, but the debug `quit` op closes it, like the
// native host (`crates/app/src/gui.rs`) behind a modal overlay. `no-socket`
// skips the debug socket, like an app that is not an omp-tui host.

import * as net from "node:net";

const mode = process.argv[2];
if (
	mode !== "repeat" &&
	mode !== "first" &&
	mode !== "never" &&
	mode !== "quit-op"
) {
	throw new Error(`fake-host: unknown mode ${JSON.stringify(mode)}`);
}
const socketPath =
	process.argv[3] === "no-socket" ? undefined : process.env.OMP_TUI_DEBUG;

let lastPress: number | undefined;

/** One `C-c`, from the debug socket or the PTY. */
function press() {
	const now = performance.now();
	const repeated = lastPress !== undefined && now - lastPress <= 500;
	lastPress = now;
	if (mode === "first" || (mode === "repeat" && repeated)) {
		process.stdin.setRawMode?.(false);
		process.exit(0);
	}
}

// Raw mode: the tool's PTY writes arrive as bytes instead of SIGINT.
process.stdin.setRawMode?.(true);
process.stdin.on("data", (chunk: Buffer) => {
	for (const byte of chunk) if (byte === 0x03) press();
});
process.stdout.write("fake host ready\r\n");

if (socketPath) {
	const server = net.createServer((sock) => {
		let pending = "";
		sock.setEncoding("utf8");
		sock.on("data", (data: string) => {
			pending += data;
			for (;;) {
				const index = pending.indexOf("\n");
				if (index < 0) return;
				const line = pending.slice(0, index);
				pending = pending.slice(index + 1);
				answer(sock, JSON.parse(line));
			}
		});
		sock.on("error", () => {});
	});
	server.listen(socketPath);
}

/**
 * Answers one request the way `crates/tui/src/debug.rs` does (`quit` in
 * `quit-op` mode: the way `crates/app/src/gui.rs` does): the reply is written
 * before the injected chords or the close reach the host.
 */
function answer(sock: net.Socket, request: { op?: string; keys?: string }) {
	let presses = 0;
	let close = false;
	let reply: Record<string, unknown>;
	switch (request.op) {
		case "text":
			reply = { ok: true, lines: ["fake host ready"], window_top: 0 };
			break;
		case "quit":
			if (mode === "quit-op") {
				close = true;
				reply = { ok: true, closed: true };
			} else {
				presses = 1;
				reply = { ok: true, injected: "C-c" };
			}
			break;
		case "keys": {
			const chords = (request.keys ?? "").split(/\s+/).filter(Boolean);
			presses = chords.filter((chord) => chord === "C-c").length;
			reply = { ok: true, injected: chords.length };
			break;
		}
		default:
			reply = { ok: false, error: `fake host: unsupported op ${request.op}` };
	}
	sock.write(`${JSON.stringify(reply)}\n`, () => {
		if (close) {
			process.stdin.setRawMode?.(false);
			process.exit(0);
		}
		for (let index = 0; index < presses; index++) press();
	});
}
