/**
 * The seam between a collab controller and whatever carries its frames.
 *
 * `CollabHost` and `CollabGuestLink` speak {@link CollabFrame}; they do not
 * care whether the bytes reach a relay over a WebSocket or a peer on the same
 * machine over a unix socket. This interface is exactly the surface the host uses,
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

	/**
	 * The transport's peer ids were reissued (the relay recreated the room), so
	 * every id the controller holds is meaningless. Transports whose ids are
	 * never reissued simply never fire it.
	 */
	onRoomRecreated?: () => void;

	connect(): void;
	/** `targetPeer` 0 broadcasts; N targets one peer. */
	send(frame: CollabFrame, targetPeer?: number): void;
	/**
	 * Queue a sequence of frames that must reach `targetPeer` contiguously,
	 * ahead of anything sent after it — a welcome's snapshot chunks.
	 */
	sendBatch(frames: Iterable<CollabFrame>, targetPeer?: number): void;
	/**
	 * False once the transport has retired `peerId`. The controller reads this
	 * instead of keeping its own departure bookkeeping, so a frame dispatched
	 * after its sender left is not acted on.
	 */
	isServing(peerId: number): boolean;
	/** Revoke frames queued but not yet handed to the wire, before a final frame. */
	discardPendingSends(): void;
	/**
	 * Resolves once regular sends queued before the call are far enough along
	 * that a following `close()` still delivers them (the host's goodbye).
	 */
	flush(): Promise<void>;
	/** Intentional close. Suppresses reconnect; a later `connect()` starts fresh. */
	close(): void;
}
