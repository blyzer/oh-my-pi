//! Guest-mutation admission: host-admitted collaboration mutations decoded
//! into the vocabulary every controller understands.
//!
//! [`omp_collab::host::HostAdmission`] authenticates a peer and refuses
//! read-only mutations before anything reaches a controller. This module owns
//! the next step for every composition that hosts a room (the chat controller
//! and the headless host alike): protobuf decoding, author attribution taken
//! only from the admitted principal, content-addressing of attached media, and
//! the mapping onto the kernel mailbox. Controllers never match on
//! `omp.collab.v1` payloads themselves.

use omp_agent::{TurnInput, Up};
use omp_collab::host::{AuthorizedMutation, RemoteOperation};
use omp_core::Str;
use omp_journal::{
	blob::{self, BlobStore},
	data::Attachment,
};
use omp_proto::collab::v1::agent_command;
use omp_session::AttachmentInput;
use thiserror::Error;

/// A guest prompt with its authenticated author, media not yet stored.
#[derive(Debug)]
pub struct GuestPrompt {
	/// User-authored text.
	pub text:   Str,
	/// Inline images in relay order, positional against `[Image #N]`.
	pub images: Vec<AttachmentInput>,
	/// Display name of the admitted principal; never taken from the payload.
	pub author: Str,
}

/// A guest prompt whose media is content-addressed in the session blob store.
pub struct StoredGuestPrompt {
	/// Turn input for an idle kernel.
	pub input:  TurnInput,
	/// Display name of the admitted principal.
	pub author: Str,
}

impl GuestPrompt {
	/// Content-addresses the images in `blobs`.
	///
	/// The blob store is shared with the session, so a caller whose kernel
	/// currently holds the session mutably passes a clone of
	/// `Session::blobs`.
	pub fn store(self, blobs: &BlobStore) -> Result<StoredGuestPrompt, blob::Error> {
		let attachments = self
			.images
			.into_iter()
			.map(|image| {
				blobs
					.put(&image.bytes)
					.map(|blob| Attachment { blob, mime: image.mime })
			})
			.collect::<Result<Vec<_>, _>>()?;
		Ok(StoredGuestPrompt {
			input:  TurnInput { text: self.text, attachments },
			author: self.author,
		})
	}
}

impl StoredGuestPrompt {
	/// Lowers the prompt to a steering aside for a running kernel turn.
	///
	/// Attribution is inserted atomically with the queued user node and never
	/// enters model-facing content.
	#[must_use]
	pub fn into_steer(self) -> Up {
		Up::SteerAuthored {
			text:        self.input.text,
			attachments: self.input.attachments,
			author:      self.author,
		}
	}
}

/// A visible-agent lifecycle command from a writable guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuestAgentCommand {
	/// Send a non-empty chat message, reviving the agent when it is parked.
	Chat(Str),
	/// Abort a running agent.
	Kill,
	/// Revive a parked or finished agent.
	Revive,
}

/// One host-admitted guest mutation.
#[derive(Debug)]
pub enum GuestAction {
	/// Submit a prompt.
	Prompt(GuestPrompt),
	/// Interrupt the host's active generation.
	Interrupt,
	/// Control one visible agent.
	Agent {
		/// Stable host agent id.
		agent_id: Str,
		/// Requested lifecycle operation.
		command:  GuestAgentCommand,
	},
}

/// A mutation the controller has nothing to do for.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GuestRejection {
	/// UI responses settle host dialogs inside the collaboration owner and
	/// never reach a controller.
	#[error("UI responses are settled by the collaboration owner")]
	UiResponse,
	/// Agent chat requires non-whitespace text.
	#[error("collaboration agent chat message is empty")]
	EmptyAgentChat,
	/// The wire carried an unknown agent command.
	#[error("collaboration agent command is unknown")]
	UnknownAgentCommand,
}

impl GuestAction {
	/// Decodes one mutation the host admission authority already admitted.
	///
	/// The read-only refusal happened in
	/// [`omp_collab::host::HostAdmission::admit_mutation`]; this function
	/// trusts that and only stamps the admitted principal as the author.
	pub fn admit(mutation: AuthorizedMutation) -> Result<Self, GuestRejection> {
		let author = Str::new(mutation.principal.display_name());
		match mutation.operation {
			RemoteOperation::Prompt(prompt) => Ok(Self::Prompt(GuestPrompt {
				text: Str::new(prompt.text),
				images: prompt
					.images
					.into_iter()
					.map(|image| AttachmentInput { mime: Str::new(image.mime_type), bytes: image.data })
					.collect(),
				author,
			})),
			RemoteOperation::Abort(_) => Ok(Self::Interrupt),
			RemoteOperation::AgentCommand(command) => {
				let agent_id = Str::new(command.agent_id.as_str());
				let command = match agent_command::Command::try_from(command.command) {
					Ok(agent_command::Command::Chat) => {
						let text = command
							.text
							.as_deref()
							.map(str::trim)
							.filter(|text| !text.is_empty())
							.ok_or(GuestRejection::EmptyAgentChat)?;
						GuestAgentCommand::Chat(Str::new(text))
					},
					Ok(agent_command::Command::Kill) => GuestAgentCommand::Kill,
					Ok(agent_command::Command::Revive) => GuestAgentCommand::Revive,
					Err(_) => return Err(GuestRejection::UnknownAgentCommand),
				};
				Ok(Self::Agent { agent_id, command })
			},
			RemoteOperation::UiResponse(_) => Err(GuestRejection::UiResponse),
		}
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;
	use omp_collab::{PROTOCOL_REVISION, crypto::WriteToken, host::HostAdmission};
	use omp_core::sf;
	use omp_proto::collab::v1::{
		AbortRequest, AgentCommand, Hello, ImageAttachment, PromptRequest, UiResponse, collab_frame,
	};

	use super::*;

	const TOKEN: [u8; 16] = [7; 16];

	fn admission() -> HostAdmission {
		HostAdmission::new(sf!("room"), WriteToken::from_bytes(TOKEN))
	}

	fn hello(name: &str, writable: bool) -> Hello {
		Hello {
			protocol_revision: PROTOCOL_REVISION,
			display_name:      name.to_owned(),
			write_token:       writable.then(|| Bytes::copy_from_slice(&TOKEN)),
			client_version:    String::new(),
		}
	}

	fn admit(name: &str, writable: bool, payload: collab_frame::Payload) -> Result<GuestAction, ()> {
		let authority = admission();
		let peer = authority
			.authenticate(3, &hello(name, writable))
			.expect("hello");
		let mutation = authority.admit_mutation(&peer, &payload).map_err(|_| ())?;
		Ok(GuestAction::admit(mutation).expect("decodable"))
	}

	#[test]
	fn prompt_author_comes_from_the_principal_and_images_keep_order() {
		let payload = collab_frame::Payload::Prompt(PromptRequest {
			text:   "look at [Image #1] and [Image #2]".to_owned(),
			images: vec![
				ImageAttachment {
					mime_type: "image/png".to_owned(),
					data:      Bytes::from_static(b"one"),
				},
				ImageAttachment {
					mime_type: "image/jpeg".to_owned(),
					data:      Bytes::from_static(b"two"),
				},
			],
		});
		let GuestAction::Prompt(prompt) = admit("  Ada  ", true, payload).expect("writable") else {
			panic!("prompt action");
		};
		assert_eq!(prompt.author.as_str(), "Ada");
		assert_eq!(prompt.images.len(), 2);
		assert_eq!(prompt.images[0].mime.as_str(), "image/png");
		assert_eq!(prompt.images[1].bytes.as_ref(), b"two");
	}

	#[test]
	fn read_only_peers_never_reach_a_controller() {
		for payload in [
			collab_frame::Payload::Prompt(PromptRequest::default()),
			collab_frame::Payload::Abort(AbortRequest::default()),
			collab_frame::Payload::AgentCommand(AgentCommand::default()),
			collab_frame::Payload::UiResponse(UiResponse::default()),
		] {
			assert!(admit("viewer", false, payload).is_err(), "read-only link must be refused");
		}
	}

	#[test]
	fn agent_chat_is_trimmed_and_empty_chat_is_rejected() {
		let chat = |text: Option<&str>| AgentCommand {
			agent_id: "child".to_owned(),
			command:  agent_command::Command::Chat as i32,
			text:     text.map(str::to_owned),
		};
		let authority = admission();
		let peer = authority
			.authenticate(3, &hello("Ada", true))
			.expect("hello");
		let admit_chat = |text: Option<&str>| {
			let mutation = authority
				.admit_mutation(&peer, &collab_frame::Payload::AgentCommand(chat(text)))
				.expect("writable");
			GuestAction::admit(mutation)
		};
		match admit_chat(Some("  hello  ")).expect("chat") {
			GuestAction::Agent { agent_id, command } => {
				assert_eq!(agent_id.as_str(), "child");
				assert_eq!(command, GuestAgentCommand::Chat(Str::new("hello")));
			},
			other => panic!("unexpected {other:?}"),
		}
		assert_eq!(admit_chat(Some("   ")).unwrap_err(), GuestRejection::EmptyAgentChat);
		assert_eq!(admit_chat(None).unwrap_err(), GuestRejection::EmptyAgentChat);
	}

	#[test]
	fn abort_maps_to_interrupt_and_stored_prompt_steers_with_attribution() {
		assert!(matches!(
			admit("Ada", true, collab_frame::Payload::Abort(AbortRequest::default())),
			Ok(GuestAction::Interrupt)
		));
		let dir = tempfile::tempdir().expect("blob dir");
		let blobs = BlobStore::open(dir.path().join("blobs")).expect("blob store");
		let stored = GuestPrompt {
			text:   Str::new("hello"),
			images: vec![AttachmentInput {
				mime:  Str::new("image/png"),
				bytes: Bytes::from_static(b"png"),
			}],
			author: Str::new("Ada"),
		}
		.store(&blobs)
		.expect("store");
		assert_eq!(stored.input.attachments.len(), 1);
		match stored.into_steer() {
			Up::SteerAuthored { text, attachments, author } => {
				assert_eq!(text.as_str(), "hello");
				assert_eq!(attachments.len(), 1);
				assert_eq!(author.as_str(), "Ada");
			},
			other => panic!("unexpected {other:?}"),
		}
	}
}
