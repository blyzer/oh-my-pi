//! In-process, content-blind collaboration relay for proofs.
//!
//! The hosted relay is not self-hostable, so joined-system tests need a real
//! socket peer that honors the same forwarding contract without any access to
//! room keys. This server speaks the browser-compatible relay grammar the
//! native [`crate::relay::RelayClient`] expects:
//!
//! - `GET /r/<room>?role=host|guest&revision=3` upgrades to a WebSocket.
//! - Envelopes are opaque: the first four bytes name the destination peer (`0`
//!   broadcasts to every guest) and the rest is ciphertext the relay never
//!   opens. Guest-to-host envelopes get the sender's relay peer id written over
//!   those four bytes.
//! - The host receives plaintext `peer-joined` / `peer-left` controls; guests
//!   receive `room-closed` and a `4001` close when the host disappears.
//! - Fatal close codes match the client's terminal table: `4004` no such room,
//!   `4009` host conflict, `4029` room full.
//!
//! Every forwarded envelope is retained so a proof can assert that plaintext
//! never crossed the relay.

use std::{
	collections::{BTreeMap, HashMap},
	io,
	net::SocketAddr,
	sync::Arc,
};

use bytes::Bytes;
use futures::{SinkExt as _, StreamExt as _};
use parking_lot::Mutex;
use thiserror::Error;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::{
	accept_hdr_async,
	tungstenite::{
		Message,
		handshake::server::{ErrorResponse, Request, Response},
		protocol::{CloseFrame, frame::coding::CloseCode},
	},
};

use crate::link::{EndpointError, RelayEndpoint};

/// Maximum concurrent guests per room, as on the hosted relay.
pub const MAX_GUESTS: usize = 32;
/// Envelopes queued per connection before the relay sheds the connection.
const OUTBOUND_QUEUE: usize = 1024;

/// One frame the relay delivered to a socket.
#[derive(Debug)]
enum Outbound {
	/// Binary envelope or text control.
	Message(Message),
	/// Close with a code, then end the connection.
	Close(u16, &'static str),
	/// Drop the socket without a close handshake.
	Sever,
}

struct Room {
	host:       Option<flume::Sender<Outbound>>,
	guests:     BTreeMap<u32, flume::Sender<Outbound>>,
	next_guest: u32,
}

#[derive(Default)]
struct State {
	rooms:     HashMap<String, Room>,
	forwarded: Vec<Bytes>,
}

/// A running in-process relay. Dropping it stops accepting and severs every
/// connection.
pub struct TestRelay {
	address: SocketAddr,
	state:   Arc<Mutex<State>>,
	accept:  JoinHandle<()>,
}

/// Failure to start the test relay.
#[derive(Debug, Error)]
pub enum TestRelayError {
	/// The loopback listener could not bind.
	#[error("test relay could not bind a loopback listener")]
	Bind(#[source] io::Error),
	/// The bound address did not form a valid relay endpoint.
	#[error("test relay address is not a valid relay endpoint")]
	Endpoint(#[from] EndpointError),
}

impl TestRelay {
	/// Binds an ephemeral loopback port and starts accepting connections.
	pub async fn start() -> Result<Self, TestRelayError> {
		let listener = TcpListener::bind(("127.0.0.1", 0))
			.await
			.map_err(TestRelayError::Bind)?;
		let address = listener.local_addr().map_err(TestRelayError::Bind)?;
		let state = Arc::new(Mutex::new(State::default()));
		let shared = Arc::clone(&state);
		let accept = tokio::spawn(async move {
			while let Ok((stream, _)) = listener.accept().await {
				tokio::spawn(serve_connection(stream, Arc::clone(&shared)));
			}
		});
		Ok(Self { address, state, accept })
	}

	/// Returns the relay origin hosts and guests dial.
	pub fn endpoint(&self) -> Result<RelayEndpoint, EndpointError> {
		RelayEndpoint::parse(&format!("ws://{}", self.address))
	}

	/// Returns every binary envelope the relay forwarded, in arrival order.
	///
	/// Envelopes are ciphertext; the relay holds no key that could open them.
	pub fn forwarded(&self) -> Vec<Bytes> {
		self.state.lock().forwarded.clone()
	}

	/// Returns the number of guests currently connected across all rooms.
	pub fn guest_count(&self) -> usize {
		self
			.state
			.lock()
			.rooms
			.values()
			.map(|room| room.guests.len())
			.sum()
	}

	/// Returns whether any room currently has a connected host.
	pub fn has_host(&self) -> bool {
		self
			.state
			.lock()
			.rooms
			.values()
			.any(|room| room.host.is_some())
	}

	/// Abruptly drops every guest socket without a close handshake, as a
	/// network partition would. Hosts stay connected.
	pub fn sever_guests(&self) {
		let state = self.state.lock();
		for room in state.rooms.values() {
			for guest in room.guests.values() {
				let _ = guest.try_send(Outbound::Sever);
			}
		}
	}
}

impl Drop for TestRelay {
	fn drop(&mut self) {
		self.accept.abort();
		let state = self.state.lock();
		for room in state.rooms.values() {
			if let Some(host) = &room.host {
				let _ = host.try_send(Outbound::Sever);
			}
			for guest in room.guests.values() {
				let _ = guest.try_send(Outbound::Sever);
			}
		}
	}
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Role {
	Host,
	Guest,
}

struct Route {
	room: String,
	role: Role,
}

fn parse_route(request: &Request) -> Option<Route> {
	let room = request.uri().path().strip_prefix("/r/")?.to_owned();
	if room.is_empty() {
		return None;
	}
	let mut role = None;
	let mut revision_ok = false;
	for pair in request.uri().query()?.split('&') {
		match pair.split_once('=')? {
			("role", "host") => role = Some(Role::Host),
			("role", "guest") => role = Some(Role::Guest),
			("revision", "3") => revision_ok = true,
			_ => {},
		}
	}
	revision_ok.then_some(())?;
	Some(Route { room, role: role? })
}

#[allow(
	clippy::result_large_err,
	reason = "tungstenite's handshake `Callback` fixes the error type it returns"
)]
async fn serve_connection(stream: tokio::net::TcpStream, state: Arc<Mutex<State>>) {
	let mut route = None;
	let callback = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
		route = parse_route(request);
		Ok(response)
	};
	let Ok(socket) = accept_hdr_async(stream, callback).await else {
		return;
	};
	let Some(route) = route else {
		let (mut sink, _) = socket.split();
		let _ = sink.close().await;
		return;
	};
	let (mut sink, mut source) = socket.split();
	let (tx, rx) = flume::bounded::<Outbound>(OUTBOUND_QUEUE);

	// Admission: decide under the lock, then act on the socket outside it.
	let admitted = {
		let mut guard = state.lock();
		match route.role {
			Role::Host => {
				let room = guard
					.rooms
					.entry(route.room.clone())
					.or_insert_with(|| Room {
						host:       None,
						guests:     BTreeMap::new(),
						next_guest: 1,
					});
				if room.host.is_some() {
					Err((4009, "host conflict"))
				} else {
					room.host = Some(tx.clone());
					Ok(0)
				}
			},
			Role::Guest => match guard.rooms.get_mut(&route.room) {
				Some(room) if room.host.is_some() => {
					if room.guests.len() >= MAX_GUESTS {
						Err((4029, "room is full"))
					} else {
						let peer = room.next_guest;
						room.next_guest += 1;
						room.guests.insert(peer, tx.clone());
						if let Some(host) = &room.host {
							let _ = host.try_send(Outbound::Message(Message::text(format!(
								r#"{{"t":"peer-joined","peer":{peer}}}"#
							))));
						}
						Ok(peer)
					}
				},
				_ => Err((4004, "no such room")),
			},
		}
	};
	let peer_id = match admitted {
		Ok(peer_id) => peer_id,
		Err((code, reason)) => {
			let _ = sink
				.send(Message::Close(Some(CloseFrame {
					code:   CloseCode::from(code),
					reason: reason.into(),
				})))
				.await;
			return;
		},
	};

	let writer = tokio::spawn(async move {
		while let Ok(outbound) = rx.recv_async().await {
			match outbound {
				Outbound::Message(message) => {
					if sink.send(message).await.is_err() {
						return;
					}
				},
				Outbound::Close(code, reason) => {
					let _ = sink
						.send(Message::Close(Some(CloseFrame {
							code:   CloseCode::from(code),
							reason: reason.into(),
						})))
						.await;
					return;
				},
				Outbound::Sever => return,
			}
		}
	});

	let mut writer = writer;
	loop {
		tokio::select! {
			message = source.next() => match message {
				Some(Ok(Message::Binary(bytes))) => forward(&state, &route, peer_id, bytes),
				Some(Ok(Message::Close(_)) | Err(_)) | None => break,
				Some(Ok(_)) => {},
			},
			// The writer ends on a close or sever instruction; dropping both
			// socket halves then ends the connection.
			_ = &mut writer => break,
		}
	}
	writer.abort();
	depart(&state, &route, peer_id);
}

/// Routes one opaque envelope. The relay reads only the four-byte peer header.
fn forward(state: &Mutex<State>, route: &Route, sender: u32, bytes: Bytes) {
	if bytes.len() < 4 {
		return;
	}
	let mut guard = state.lock();
	guard.forwarded.push(bytes.clone());
	let Some(room) = guard.rooms.get(&route.room) else {
		return;
	};
	match route.role {
		Role::Host => {
			let destination = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
			if destination == 0 {
				for guest in room.guests.values() {
					let _ = guest.try_send(Outbound::Message(Message::Binary(bytes.clone())));
				}
			} else if let Some(guest) = room.guests.get(&destination) {
				let _ = guest.try_send(Outbound::Message(Message::Binary(bytes)));
			}
		},
		Role::Guest => {
			let mut rewritten = bytes.to_vec();
			rewritten[..4].copy_from_slice(&sender.to_be_bytes());
			if let Some(host) = &room.host {
				let _ = host.try_send(Outbound::Message(Message::Binary(rewritten.into())));
			}
		},
	}
}

/// Retires a connection: a guest leaves with a `peer-left` control, a host
/// takes its room down and closes every guest with `4001`.
fn depart(state: &Mutex<State>, route: &Route, peer_id: u32) {
	let mut guard = state.lock();
	match route.role {
		Role::Guest => {
			if let Some(room) = guard.rooms.get_mut(&route.room) {
				room.guests.remove(&peer_id);
				if let Some(host) = &room.host {
					let _ = host.try_send(Outbound::Message(Message::text(format!(
						r#"{{"t":"peer-left","peer":{peer_id}}}"#
					))));
				}
			}
		},
		Role::Host => {
			if let Some(room) = guard.rooms.remove(&route.room) {
				for guest in room.guests.values() {
					let _ = guest.try_send(Outbound::Message(Message::text(r#"{"t":"room-closed"}"#)));
					let _ = guest.try_send(Outbound::Close(4001, "room closed"));
				}
			}
		},
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use omp_proto::collab::v1::{Hello, PromptRequest, collab_frame};

	use super::*;
	use crate::{
		PROTOCOL_REVISION,
		codec::RelayRoute,
		crypto::{RoomId, RoomKey},
		relay::{Handshake, RelayClient, RelayError, RelayInbound, RelayRole},
	};

	const WAIT: Duration = Duration::from_secs(10);
	const SECRET: &str = "a prompt the relay must never be able to read";

	fn client(relay: &TestRelay, room: &RoomId, role: RelayRole, key: [u8; 32]) -> RelayClient {
		let url = relay.endpoint().expect("endpoint").room_url(room);
		RelayClient::new(url, role, RoomKey::from_bytes(key).expect("key")).expect("client")
	}

	async fn receive(client: &mut RelayClient) -> Result<Option<RelayInbound>, RelayError> {
		tokio::time::timeout(WAIT, client.receive())
			.await
			.expect("relay delivery within the bounded wait")
	}

	fn prompt(sequence: u64) -> omp_proto::collab::v1::CollabFrame {
		omp_proto::collab::v1::CollabFrame {
			protocol_revision: PROTOCOL_REVISION,
			sequence,
			payload: Some(collab_frame::Payload::Prompt(PromptRequest {
				text:   SECRET.to_owned(),
				images: Vec::new(),
			})),
			..Default::default()
		}
	}

	#[tokio::test]
	async fn envelopes_route_by_peer_and_the_relay_never_sees_plaintext() {
		let relay = TestRelay::start().await.expect("relay");
		let room = RoomId::generate().expect("room");
		let (_, key) = RoomKey::generate().expect("key");
		let mut host = client(&relay, &room, RelayRole::Host, key);
		host.connect().await.expect("host connects");
		let mut first = client(&relay, &room, RelayRole::Guest, key);
		first.connect().await.expect("first guest connects");
		let mut second = client(&relay, &room, RelayRole::Guest, key);
		second.connect().await.expect("second guest connects");

		let mut joined = Vec::new();
		while joined.len() < 2 {
			match receive(&mut host).await.expect("host receive") {
				Some(RelayInbound::PeerJoined(peer)) => joined.push(peer.peer_id),
				other => panic!("expected peer-joined, got {other:?}"),
			}
		}
		assert_eq!(joined, [1, 2], "peer ids are assigned in join order");

		// A guest's envelope reaches only the host, stamped with the sender's peer id.
		let hello =
			Handshake::hello(1, Hello { protocol_revision: PROTOCOL_REVISION, ..Default::default() });
		second
			.send(RelayRoute { peer_id: 0 }, &hello)
			.await
			.expect("send");
		let Some(RelayInbound::Frame(routed)) = receive(&mut host).await.expect("host receive")
		else {
			panic!("host should receive the hello frame");
		};
		assert_eq!(routed.route.peer_id, 2);
		assert!(matches!(routed.frame.payload, Some(collab_frame::Payload::Hello(_))));

		// A targeted host envelope reaches exactly one guest; broadcast reaches both.
		host
			.send(RelayRoute { peer_id: 1 }, &prompt(2))
			.await
			.expect("targeted");
		host
			.send(RelayRoute { peer_id: 0 }, &prompt(3))
			.await
			.expect("broadcast");
		let mut received = [0_usize; 2];
		for (index, guest) in [&mut first, &mut second].into_iter().enumerate() {
			while received[index] < 1 + usize::from(index == 0) {
				let Some(RelayInbound::Frame(routed)) = receive(guest).await.expect("guest receive")
				else {
					panic!("guest should receive frames only");
				};
				assert!(matches!(routed.frame.payload, Some(collab_frame::Payload::Prompt(_))));
				received[index] += 1;
			}
		}
		assert_eq!(received, [2, 1]);

		// Content-blind: every forwarded envelope is ciphertext.
		let forwarded = relay.forwarded();
		assert_eq!(forwarded.len(), 3);
		for envelope in forwarded {
			assert!(
				!envelope
					.windows(SECRET.len())
					.any(|window| window == SECRET.as_bytes()),
				"plaintext crossed the relay"
			);
		}
	}

	#[tokio::test]
	async fn admission_failures_use_the_terminal_close_codes() {
		let relay = TestRelay::start().await.expect("relay");
		let room = RoomId::generate().expect("room");
		let (_, key) = RoomKey::generate().expect("key");

		let mut orphan = client(&relay, &room, RelayRole::Guest, key);
		orphan
			.connect()
			.await
			.expect("upgrade succeeds before admission");
		assert!(matches!(receive(&mut orphan).await, Err(RelayError::FatalClose { code: 4004, .. })));

		let mut host = client(&relay, &room, RelayRole::Host, key);
		host.connect().await.expect("host connects");
		let mut usurper = client(&relay, &room, RelayRole::Host, key);
		usurper
			.connect()
			.await
			.expect("upgrade succeeds before admission");
		assert!(matches!(
			receive(&mut usurper).await,
			Err(RelayError::FatalClose { code: 4009, .. })
		));
		assert!(relay.has_host(), "the original host keeps the room");
	}

	#[tokio::test]
	async fn departures_are_announced_and_a_departing_host_closes_the_room() {
		let relay = TestRelay::start().await.expect("relay");
		let room = RoomId::generate().expect("room");
		let (_, key) = RoomKey::generate().expect("key");
		let mut host = client(&relay, &room, RelayRole::Host, key);
		host.connect().await.expect("host connects");
		let mut guest = client(&relay, &room, RelayRole::Guest, key);
		guest.connect().await.expect("guest connects");
		let Some(RelayInbound::PeerJoined(joined)) = receive(&mut host).await.expect("join") else {
			panic!("expected peer-joined");
		};

		guest.close().await.expect("guest leaves");
		let Some(RelayInbound::PeerLeft(left)) = receive(&mut host).await.expect("leave") else {
			panic!("expected peer-left");
		};
		assert_eq!(left.peer_id, joined.peer_id);
		assert_eq!(relay.guest_count(), 0);

		let mut stayer = client(&relay, &room, RelayRole::Guest, key);
		stayer.connect().await.expect("second guest connects");
		let Some(RelayInbound::PeerJoined(_)) = receive(&mut host).await.expect("join") else {
			panic!("expected peer-joined");
		};
		host.close().await.expect("host leaves");
		assert!(matches!(receive(&mut stayer).await, Err(RelayError::FatalClose { code: 4001, .. })));
		assert!(!relay.has_host());
	}

	#[tokio::test]
	async fn severed_guests_drop_without_a_close_handshake_and_the_host_is_told() {
		let relay = TestRelay::start().await.expect("relay");
		let room = RoomId::generate().expect("room");
		let (_, key) = RoomKey::generate().expect("key");
		let mut host = client(&relay, &room, RelayRole::Host, key);
		host.connect().await.expect("host connects");
		let mut guest = client(&relay, &room, RelayRole::Guest, key);
		guest.connect().await.expect("guest connects");
		let Some(RelayInbound::PeerJoined(_)) = receive(&mut host).await.expect("join") else {
			panic!("expected peer-joined");
		};
		relay.sever_guests();
		assert!(matches!(receive(&mut host).await, Ok(Some(RelayInbound::PeerLeft(_)))));
		assert!(
			matches!(receive(&mut guest).await, Ok(None)),
			"an abrupt drop is a reconnectable end"
		);
	}
}
