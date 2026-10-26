//! Read-only attachment to a session this host already runs.
//!
//! `omp attach` dials the owner socket, folds the frames `SessionWire` serves
//! into a local [`Dom`] replica, and renders that replica through the
//! production transcript projection as it converges. R1 is read-only: the
//! client sends one `Attach` and never a mutating verb.
//!
//! The replica is rendered as a tail. Every repaint holds back the last line,
//! because a streaming assistant rewrites it on every token and a terminal
//! cannot unprint. A `Resync` discards the replica and the frontier with it,
//! which is what makes a reprint correct rather than a duplicate.

use std::{
	io::{self, Write as _},
	path::{Path, PathBuf},
	time::Duration,
};

use miette::{Context as _, IntoDiagnostic as _, miette};
use omp_core::Str;
use omp_dom::Dom;
use omp_proto::session::v1 as pb;

use crate::cli::AttachArgs;

/// Renders a session this host serves, or lists the sessions it serves.
///
/// # Errors
/// Returns a diagnostic when the socket cannot be dialled, when the host
/// refuses the attachment, or when a frame does not decode.
pub async fn run(args: AttachArgs, data_dir: &Path) -> miette::Result<()> {
	let socket = args
		.socket
		.clone()
		.unwrap_or_else(|| omp_driver::session_wire::socket_path(data_dir));
	let channel = omp_rpc::uds::connect(&socket)
		.await
		.into_diagnostic()
		.wrap_err_with(|| format!("no session host answers {}", socket.display()))?;
	let mut client = pb::session_control_client::SessionControlClient::new(channel);

	let Some(session) = args.session.clone() else {
		return list(&mut client, args.project.as_deref()).await;
	};
	if !args.observe {
		return Err(miette!(
			"this build attaches read-only; pass --observe to confirm the attachment steers nothing"
		));
	}
	observe(&mut client, session, &args).await
}

/// Prints one line per session the host serves.
async fn list(
	client: &mut pb::session_control_client::SessionControlClient<tonic::transport::Channel>,
	project: Option<&Path>,
) -> miette::Result<()> {
	let request = pb::ListSessionsRequest {
		project_root: project
			.map(|root| root.display().to_string())
			.unwrap_or_default(),
	};
	let sessions = client
		.list_sessions(request)
		.await
		.into_diagnostic()?
		.into_inner()
		.sessions;
	if sessions.is_empty() {
		return Err(miette!("this host serves no session"));
	}
	let mut out = io::stdout().lock();
	for session in sessions {
		writeln!(
			out,
			"{}\t{}\t{}\tepoch {}",
			session.session_id, session.label, session.project_root, session.epoch
		)
		.into_diagnostic()?;
	}
	Ok(())
}

/// Folds the attachment into a replica and renders it as it converges.
async fn observe(
	client: &mut pb::session_control_client::SessionControlClient<tonic::transport::Channel>,
	session: Str,
	args: &AttachArgs,
) -> miette::Result<()> {
	use futures::StreamExt as _;

	let (resume_epoch, resume_seq) = parse_cursor(args.resume.as_deref())?;
	let mut stream = client
		.attach(pb::AttachRequest {
			session_id: session.as_str().to_owned(),
			resume_epoch,
			resume_seq,
		})
		.await
		.into_diagnostic()
		.wrap_err("the host refused the attachment")?
		.into_inner();

	let mut replica = Replica::default();
	let mut view = Tail::default();
	while let Some(frame) = stream.next().await {
		let frame = frame
			.into_diagnostic()
			.wrap_err("the attachment was truncated")?;
		let Some(body) = frame.frame else {
			continue;
		};
		if args.json {
			emit_json(&body)?;
			continue;
		}
		match replica.fold(body)? {
			Folded::Quiet => {},
			Folded::Reset => view.reset(),
			Folded::Changed => {},
		}
		if let Some(dom) = replica.dom() {
			view.advance(crate::print_mode::transcript_text(dom))?;
		}
	}
	view.finish()?;
	Ok(())
}

/// What one folded frame did to the replica.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Folded {
	/// Nothing the reader can see changed.
	Quiet,
	/// The replica was discarded; what was already printed is stale.
	Reset,
	/// The replica advanced.
	Changed,
}

/// The local DOM replica an attachment folds into.
#[derive(Default)]
struct Replica {
	dom:     Option<Dom>,
	/// Snapshot chunks received so far, awaiting the final one.
	partial: Vec<u8>,
}

impl Replica {
	/// The replica, once a complete snapshot has arrived.
	const fn dom(&self) -> Option<&Dom> {
		self.dom.as_ref()
	}

	/// Applies one frame.
	///
	/// # Errors
	/// Returns a diagnostic when a snapshot or an event does not decode, which
	/// means this client and the host disagree about the wire.
	fn fold(&mut self, frame: pb::attach_frame::Frame) -> miette::Result<Folded> {
		match frame {
			pb::attach_frame::Frame::Welcome(welcome) => {
				eprintln!(
					"attached to {} at epoch {} seq {}",
					welcome.label, welcome.epoch, welcome.head_seq
				);
				Ok(Folded::Quiet)
			},
			pb::attach_frame::Frame::SnapshotChunk(chunk) => {
				self.partial.extend_from_slice(&chunk.bytes);
				if !chunk.r#final {
					return Ok(Folded::Quiet);
				}
				let snapshot = omp_dom::Snapshot::from_bytes(&self.partial)
					.into_diagnostic()
					.wrap_err("the host sent a snapshot this build cannot read")?;
				self.partial.clear();
				self.dom = Some(Dom::from_snapshot(&snapshot));
				Ok(Folded::Changed)
			},
			pb::attach_frame::Frame::Event(event) => {
				let Some(dom) = self.dom.as_mut() else {
					// An event before the snapshot completes cannot be
					// applied; the host sends none, so this is a wire defect
					// rather than a race the client should paper over.
					return Err(miette!("the host sent an event before its snapshot"));
				};
				let decoded = omp_dom::wire::decode_event(&event.bytes)
					.into_diagnostic()
					.wrap_err("the host sent an event this build cannot read")?;
				dom.apply_event(&decoded)
					.into_diagnostic()
					.wrap_err("the replica refused an event the host published")?;
				Ok(Folded::Changed)
			},
			pb::attach_frame::Frame::Resync(resync) => {
				eprintln!("resynchronizing: {}", resync.reason);
				self.dom = None;
				self.partial.clear();
				Ok(Folded::Reset)
			},
			pb::attach_frame::Frame::Heartbeat(_) => Ok(Folded::Quiet),
		}
	}
}

/// The rendered prefix already written to standard output.
#[derive(Default)]
struct Tail {
	/// Lines printed so far.
	printed: usize,
	/// The last rendering, held so the final line can be flushed at the end.
	last:    String,
}

impl Tail {
	/// Forgets the printed frontier after a resync, so the fresh replica
	/// reprints in full under its own heading.
	fn reset(&mut self) {
		self.printed = 0;
		self.last.clear();
	}

	/// Prints every settled line of `rendered` that has not been printed.
	///
	/// The last line is held back: a streaming assistant rewrites it on every
	/// token, and standard output cannot take a line back.
	fn advance(&mut self, rendered: String) -> miette::Result<()> {
		let settled = rendered.lines().count().saturating_sub(1);
		if settled > self.printed {
			let mut out = io::stdout().lock();
			for line in rendered.lines().take(settled).skip(self.printed) {
				writeln!(out, "{line}").into_diagnostic()?;
			}
			out.flush().into_diagnostic()?;
			self.printed = settled;
		}
		self.last = rendered;
		Ok(())
	}

	/// Flushes the line held back by the last advance.
	fn finish(&mut self) -> miette::Result<()> {
		let mut out = io::stdout().lock();
		for line in self.last.lines().skip(self.printed) {
			writeln!(out, "{line}").into_diagnostic()?;
		}
		out.flush().into_diagnostic()?;
		Ok(())
	}
}

/// Writes one frame as a JSON line, for a consumer that folds frames itself.
fn emit_json(frame: &pb::attach_frame::Frame) -> miette::Result<()> {
	let line = match frame {
		pb::attach_frame::Frame::Welcome(welcome) => serde_json::json!({
			"frame": "welcome",
			"epoch": welcome.epoch,
			"head_seq": welcome.head_seq,
			"label": welcome.label,
			"granted_scopes": welcome.granted_scopes,
		}),
		pb::attach_frame::Frame::SnapshotChunk(chunk) => serde_json::json!({
			"frame": "snapshot",
			"epoch": chunk.epoch,
			"seq": chunk.seq,
			"index": chunk.index,
			"final": chunk.r#final,
			"bytes": chunk.bytes.len(),
		}),
		pb::attach_frame::Frame::Event(event) => serde_json::json!({
			"frame": "event",
			"epoch": event.epoch,
			"seq": event.seq,
			"bytes": event.bytes.len(),
		}),
		pb::attach_frame::Frame::Resync(resync) => serde_json::json!({
			"frame": "resync",
			"reason": resync.reason,
		}),
		pb::attach_frame::Frame::Heartbeat(beat) => serde_json::json!({
			"frame": "heartbeat",
			"head_seq": beat.head_seq,
		}),
	};
	let mut out = io::stdout().lock();
	writeln!(out, "{line}").into_diagnostic()?;
	out.flush().into_diagnostic()
}

/// Parses an `<epoch>:<seq>` resume cursor.
///
/// # Errors
/// Returns a diagnostic when either half is not a number, because a silently
/// ignored cursor would resend a whole snapshot without saying so.
fn parse_cursor(cursor: Option<&str>) -> miette::Result<(u64, u64)> {
	let Some(cursor) = cursor else {
		return Ok((0, 0));
	};
	let (epoch, seq) = cursor
		.split_once(':')
		.ok_or_else(|| miette!("a resume cursor is <epoch>:<seq>, got {cursor}"))?;
	let epoch = epoch
		.parse()
		.map_err(|_| miette!("a resume cursor is <epoch>:<seq>, got {cursor}"))?;
	let seq = seq
		.parse()
		.map_err(|_| miette!("a resume cursor is <epoch>:<seq>, got {cursor}"))?;
	Ok((epoch, seq))
}

/// Idle period the host's heartbeat guarantees, used by callers that bound a
/// silent attachment.
pub const HEARTBEAT_GRACE: Duration = Duration::from_secs(45);

#[cfg(test)]
mod tests {
	use omp_dom::{KnownTag, NodeSpec, Op, PropId, Txn, Value};
	use omp_journal::EntryId;

	use super::*;

	/// Builds a session tree with one assistant line.
	fn seeded() -> (Dom, flume::Receiver<omp_dom::Event>) {
		let mut dom = Dom::new();
		let body = dom.body();
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: None,
			ops:   vec![Op::Ins { parent: body, after: None, node: NodeSpec::new(KnownTag::Turn) }],
		})
		.expect("a turn inserts under the body");
		let turn = dom.children(body).last().copied().expect("the seeded turn");
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: None,
			ops:   vec![Op::Ins {
				parent: turn,
				after:  None,
				node:   NodeSpec::new(KnownTag::Assistant)
					.with_prop(PropId::Text, Value::Str(Str::new_static("first"))),
			}],
		})
		.expect("an assistant inserts under the turn");
		let (_, events) = dom.subscribe();
		(dom, events)
	}

	fn chunks(dom: &Dom) -> Vec<pb::attach_frame::Frame> {
		let snapshot = dom.snapshot();
		vec![pb::attach_frame::Frame::SnapshotChunk(pb::SnapshotChunk {
			epoch:   1,
			seq:     0,
			index:   0,
			r#final: true,
			bytes:   snapshot.as_bytes().to_vec().into(),
		})]
	}

	#[test]
	fn a_snapshot_then_an_event_converges_on_the_host_tree() {
		let (mut dom, events) = seeded();
		let mut replica = Replica::default();
		for frame in chunks(&dom) {
			replica.fold(frame).expect("the snapshot folds");
		}
		assert_eq!(
			crate::print_mode::transcript_text(replica.dom().expect("the replica exists")),
			crate::print_mode::transcript_text(&dom),
			"the replica starts at the host tree"
		);

		let turn = dom
			.children(dom.body())
			.last()
			.copied()
			.expect("the seeded turn");
		dom.apply(&Txn {
			cause: EntryId::default(),
			label: None,
			ops:   vec![Op::Ins {
				parent: turn,
				after:  None,
				node:   NodeSpec::new(KnownTag::Assistant)
					.with_prop(PropId::Text, Value::Str(Str::new_static("second"))),
			}],
		})
		.expect("a second assistant inserts");
		for event in events.try_iter() {
			let bytes = omp_dom::wire::encode_event(&event).expect("the event encodes");
			let folded = replica
				.fold(pb::attach_frame::Frame::Event(pb::EventFrame {
					epoch: 1,
					seq:   1,
					bytes: bytes.into(),
				}))
				.expect("the event folds");
			assert_eq!(folded, Folded::Changed);
		}
		assert_eq!(
			crate::print_mode::transcript_text(replica.dom().expect("the replica exists")),
			crate::print_mode::transcript_text(&dom),
			"the replica follows the host tree"
		);
	}

	#[test]
	fn a_resync_discards_the_replica() {
		let (dom, _) = seeded();
		let mut replica = Replica::default();
		for frame in chunks(&dom) {
			replica.fold(frame).expect("the snapshot folds");
		}
		assert!(replica.dom().is_some(), "the snapshot built a replica");

		let folded = replica
			.fold(pb::attach_frame::Frame::Resync(pb::ResyncFrame { reason: "epoch".to_owned() }))
			.expect("the resync folds");
		assert_eq!(folded, Folded::Reset);
		assert!(replica.dom().is_none(), "a resync discards the tree the client can no longer trust");
	}

	#[test]
	fn an_event_before_the_snapshot_is_a_wire_defect() {
		let mut replica = Replica::default();
		let error = replica
			.fold(pb::attach_frame::Frame::Event(pb::EventFrame {
				epoch: 1,
				seq:   1,
				bytes: bytes::Bytes::new(),
			}))
			.expect_err("an event without a replica cannot apply");
		assert!(
			format!("{error}").contains("before its snapshot"),
			"the client names the wire defect, got {error}"
		);
	}

	#[test]
	fn a_partial_snapshot_is_held_until_its_final_chunk() {
		let (dom, _) = seeded();
		let snapshot = dom.snapshot();
		let bytes = snapshot.as_bytes();
		let split = bytes.len() / 2;
		let mut replica = Replica::default();
		replica
			.fold(pb::attach_frame::Frame::SnapshotChunk(pb::SnapshotChunk {
				epoch:   1,
				seq:     0,
				index:   0,
				r#final: false,
				bytes:   bytes[..split].to_vec().into(),
			}))
			.expect("the first chunk folds");
		assert!(replica.dom().is_none(), "half a snapshot builds nothing");
		replica
			.fold(pb::attach_frame::Frame::SnapshotChunk(pb::SnapshotChunk {
				epoch:   1,
				seq:     0,
				index:   1,
				r#final: true,
				bytes:   bytes[split..].to_vec().into(),
			}))
			.expect("the final chunk folds");
		assert_eq!(
			crate::print_mode::transcript_text(replica.dom().expect("the replica exists")),
			crate::print_mode::transcript_text(&dom),
			"the reassembled snapshot is the host tree"
		);
	}

	#[test]
	fn the_tail_holds_the_last_line_until_it_settles() {
		let mut tail = Tail::default();
		tail
			.advance("one\ntwo\n".to_owned())
			.expect("the tail prints");
		assert_eq!(tail.printed, 1, "the last line is held back");
		tail
			.advance("one\ntwo\nthree\n".to_owned())
			.expect("the tail prints");
		assert_eq!(tail.printed, 2, "a settled line prints exactly once");
		tail.reset();
		assert_eq!(tail.printed, 0, "a resync forgets the frontier");
	}

	#[test]
	fn a_cursor_is_epoch_and_seq() {
		assert_eq!(parse_cursor(None).expect("no cursor is zero"), (0, 0));
		assert_eq!(parse_cursor(Some("7:12")).expect("a cursor parses"), (7, 12));
		assert!(parse_cursor(Some("7")).is_err(), "a cursor without a seq is refused");
		assert!(parse_cursor(Some("a:b")).is_err(), "a cursor that is not numeric is refused");
	}
}
