//! Headless collaboration hosting: one room over a live kernel session.
//!
//! `print`, `rpc`, and the future daemon have no chat controller, yet a factory
//! run or a shared session should still be spectatable and, for writable
//! guests, drivable. [`HeadlessRoom`] owns the relay session for a kernel the
//! caller composed, publishes it to the local host registry, and serves guest
//! mutations through the same [`GuestAction`] admission the chat controller
//! uses: an idle prompt starts an authored turn, a prompt during a turn
//! steers it with attribution, and an interrupt reaches the kernel mailbox.
//! The read-only refusal already happened in the collaboration owner's host
//! admission, so a viewer's mutation never reaches this loop.

use std::path::PathBuf;

use omp_agent::{Inference, Kernel, KernelError, RunControl, TurnOutcome, Up};
use omp_collab::{link::RelayEndpoint, presence::ConnectionState};
use omp_core::Str;
use omp_journal::blob::BlobStore;
use omp_proto::collab::v1::SessionStateUpdate;
use omp_session::Session;
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
	admission::{GuestAction, StoredGuestPrompt},
	observer::HostAgentBridge,
	registry::Access,
	session::{
		CollabCommandFault, CollabCommandHandle, CollabOwnerCommand, CollabSessionAuthority,
		spawn_session_owner,
	},
};

/// How to host a room for one session.
pub struct RoomOptions {
	/// Relay origin the room is created on.
	pub relay:        RelayEndpoint,
	/// Id of the hosted session, published to the local host registry.
	pub session_id:   Str,
	/// Highest access the local host registry hands out for this room.
	pub access:       Access,
	/// Child-transcript subscription authority for guest agent views.
	pub agents:       HostAgentBridge,
	/// Session facts guests and the registry show (name, cwd, model).
	pub state:        SessionStateUpdate,
	/// Local host-registry directory; `None` keeps the room undiscoverable.
	pub registry_dir: Option<PathBuf>,
}

/// Why [`HeadlessRoom::serve`] returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServeEnd {
	/// The caller's shutdown token fired.
	Shutdown,
	/// The relay closed the room for good or the collaboration owner stopped.
	RoomEnded,
}

/// A hosted room and the tasks that keep it alive.
pub struct HeadlessRoom {
	handle:      CollabCommandHandle,
	owner:       JoinHandle<()>,
	editor_link: Str,
	viewer_link: Str,
	state:       SessionStateUpdate,
}

/// Failure to host or serve a headless room.
#[derive(Debug, Error)]
pub enum HostError {
	/// The relay or collaboration owner refused the request.
	#[error("collaboration owner request failed")]
	Owner(#[from] CollabCommandFault),
	/// The owner started a room but published no links.
	#[error("collaboration owner started a room without links")]
	MissingLinks,
	/// A kernel turn failed for a reason the journal could not absorb.
	#[error("hosted kernel turn failed")]
	Turn(#[from] KernelError),
}

impl HeadlessRoom {
	/// Starts hosting `session` on `options.relay`.
	///
	/// The snapshot and its ordered patch stream are captured together, so a
	/// guest that joins later converges from the same journal point the host
	/// continues from.
	pub async fn open(session: &mut Session, options: RoomOptions) -> Result<Self, HostError> {
		let RoomOptions { relay, session_id, access, agents, state, registry_dir } = options;
		let (mut authority, handle) = CollabSessionAuthority::new();
		if let Some(dir) = registry_dir
			&& let Err(error) = authority.publish_to(dir)
		{
			// Discovery is a convenience; the room is still hosted.
			tracing::warn!(%error, "collaboration host registry is unavailable");
		}
		let owner = spawn_session_owner(authority);
		handle.publish_state(state.clone());
		let (snapshot, events) = session.subscribe();
		let started = handle
			.request(CollabOwnerCommand::Start { relay, snapshot, events, agents, session_id, access })
			.await;
		let links = started.map_err(HostError::from).and_then(|result| {
			result
				.editor_link
				.zip(result.viewer_link)
				.ok_or(HostError::MissingLinks)
		});
		match links {
			Ok((editor_link, viewer_link)) => {
				Ok(Self { handle, owner, editor_link, viewer_link, state })
			},
			Err(error) => {
				drop(handle);
				let _ = owner.await;
				Err(error)
			},
		}
	}

	/// The writable-guest link, joinable with `omp join`.
	#[must_use]
	pub const fn editor_link(&self) -> &Str {
		&self.editor_link
	}

	/// The read-only viewer link, joinable with `omp join`.
	#[must_use]
	pub const fn viewer_link(&self) -> &Str {
		&self.viewer_link
	}

	/// The owner's command, presence, and mutation projection.
	#[must_use]
	pub const fn handle(&self) -> &CollabCommandHandle {
		&self.handle
	}

	/// Ends the room: guests are disconnected and the registry entry is
	/// withdrawn.
	pub async fn stop(self) {
		let _ = self.handle.request(CollabOwnerCommand::Leave).await;
		// Other clones of the handle may outlive the room; the owner has no
		// active session left, so stopping it does not wait for them.
		self.owner.abort();
		let _ = self.owner.await;
	}

	fn set_streaming(&mut self, streaming: bool) {
		self.state.is_streaming = streaming;
		self.handle.publish_state(self.state.clone());
	}

	/// Serves guest mutations until `shutdown` fires or the room ends.
	///
	/// Prompts run as authored turns on `kernel`; the caller keeps the session
	/// and kernel and remains their only writer.
	#[expect(
		clippy::future_not_send,
		reason = "generic over the caller's inference client, whose `Sync`-ness is not required"
	)]
	pub async fn serve<C: Inference>(
		&mut self,
		kernel: &mut Kernel<C>,
		session: &mut Session,
		shutdown: &CancellationToken,
	) -> Result<ServeEnd, HostError> {
		let remote = self.handle.remote_mutations();
		let mut presence = self.handle.subscribe_presence();
		loop {
			tokio::select! {
				biased;
				() = shutdown.cancelled() => return Ok(ServeEnd::Shutdown),
				changed = presence.changed() => {
					let ended = changed.is_err()
						|| presence
							.borrow()
							.is_none_or(|facts| facts.connection() == ConnectionState::Disconnected);
					if ended {
						return Ok(ServeEnd::RoomEnded);
					}
				},
				mutation = remote.recv_async() => {
					let Ok(mutation) = mutation else {
						return Ok(ServeEnd::RoomEnded);
					};
					let action = match GuestAction::admit(mutation) {
						Ok(action) => action,
						Err(rejection) => {
							tracing::debug!(%rejection, "guest mutation ignored");
							continue;
						},
					};
					match action {
						GuestAction::Prompt(prompt) => {
							match prompt.store(session.blobs()) {
								Ok(stored) => {
										self.run_turn(kernel, session, stored, shutdown).await?;
									},
								Err(error) => {
									tracing::warn!(%error, "guest prompt dropped: attachments could not be stored");
								},
							}
						},
						// Nothing runs while idle, so there is nothing to interrupt.
						GuestAction::Interrupt => {},
						GuestAction::Agent { .. } => warn_agent_unsupported(),
					}
				},
			}
		}
	}

	#[expect(
		clippy::future_not_send,
		reason = "generic over the caller's inference client, whose `Sync`-ness is not required"
	)]
	async fn run_turn<C: Inference>(
		&mut self,
		kernel: &mut Kernel<C>,
		session: &mut Session,
		stored: StoredGuestPrompt,
		shutdown: &CancellationToken,
	) -> Result<TurnOutcome, HostError> {
		let remote = self.handle.remote_mutations();
		// The kernel holds the session for the turn; media steered in meanwhile
		// is content-addressed through this handle to the same store.
		let blobs = session.blobs().clone();
		let mailbox = kernel.mailbox();
		let cancel = CancellationToken::new();
		self.set_streaming(true);
		let result = {
			let control = RunControl::new(cancel.clone(), None);
			let turn = kernel.run_authored_turn(session, stored.input, stored.author, control);
			tokio::pin!(turn);
			let mut cancelled = false;
			loop {
				tokio::select! {
					biased;
					() = shutdown.cancelled(), if !cancelled => {
						cancelled = true;
						cancel.cancel();
					},
					mutation = remote.recv_async() => {
						if let Ok(mutation) = mutation {
							route_running(mutation, &blobs, &mailbox);
						}
					},
					result = &mut turn => break result,
				}
			}
		};
		self.set_streaming(false);
		result.map_err(HostError::from)
	}
}

fn warn_agent_unsupported() {
	tracing::warn!("guest agent command ignored: a headless host supervises no child agents");
}

/// Routes one guest mutation to a running kernel turn.
fn route_running(
	mutation: omp_collab::host::AuthorizedMutation,
	blobs: &BlobStore,
	mailbox: &flume::Sender<Up>,
) {
	let action = match GuestAction::admit(mutation) {
		Ok(action) => action,
		Err(rejection) => {
			tracing::debug!(%rejection, "guest mutation ignored");
			return;
		},
	};
	match action {
		GuestAction::Prompt(prompt) => match prompt.store(blobs) {
			Ok(stored) => {
				let _ = mailbox.send(stored.into_steer());
			},
			Err(error) => {
				tracing::warn!(%error, "guest steer dropped: attachments could not be stored");
			},
		},
		GuestAction::Interrupt => {
			let _ = mailbox.send(Up::Interrupt);
		},
		GuestAction::Agent { .. } => warn_agent_unsupported(),
	}
}
