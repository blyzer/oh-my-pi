/**
 * The seam between a collab controller and whatever carries its frames.
 *
 * `CollabHost` and `CollabGuestLink` speak {@link CollabFrame}; they do not
 * care whether the bytes reach a relay over a WebSocket or a peer on the same
 * machine over a unix socket. This interface is exactly the surface they use,
 * so a second transport is a new implementation rather than a change to either
 * controller.
 *
 * Framing concerns live BELOW this line: sealing, envelope packing and peer-id
 * rewriting are the transport's business ({@link CollabSocket} does all three).
 * A local transport that is already confined to one machine may pass frames in
 * the clear, because there is no relay operator in the middle to keep out.
 */
import type { RelayControlMessage } from "@oh-my-pi/pi-wire";
import type { CollabFrame } from "./protocol";

export interface CollabTransport {
	/** Fires after every successful (re)connect, not only the first. */
	onOpen?: () => void;
	/** A decoded frame arrived. `fromPeer` is 0 for host-authored traffic. */
	onFrame?: (frame: CollabFrame, fromPeer: number) => void;
	/** Out-of-band transport control (peer joined/left), where the transport has it. */
	onControl?: (msg: RelayControlMessage) => void;
	/**
	 * Fires once per terminal close. `willReconnect` is true for a transient
	 * drop the transport intends to retry, so a controller can report "lost,
	 * reconnecting" instead of tearing its taps down.
	 */
	onClose?: (reason: string, willReconnect: boolean) => void;

	connect(): void;
	/** `targetPeer` 0 broadcasts; N targets one peer. */
	send(frame: CollabFrame, targetPeer?: number): void;
	/** Intentional close. Suppresses reconnect; a later `connect()` starts fresh. */
	close(): void;
}
