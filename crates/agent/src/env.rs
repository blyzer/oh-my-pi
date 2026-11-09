//! Environment-to-kernel events carried by the single upward mailbox.

use std::fmt::Write as _;

pub use omp_contract::EnvEvent;
use omp_core::{Str, StrMut};
use omp_dom::{Handle, KnownTag, NodeSpec, Op, PropId, PropKey, Value};
use omp_journal::data::{IrcTraffic, LaunchCompletion, LaunchDaemonCompletion};
use omp_session::{Session, SessionError};

/// Builds the atomic journal operations for a supervised-process settlement.
///
/// The completion row and the job's delivery marker are committed in the same
/// `jobs.settle` patch. A crash therefore observes both or neither, and replay
/// cannot inject a second row for the process through generic async delivery.
pub fn launch_completion_ops(
	session: &Session,
	job: Handle,
	completion: &LaunchDaemonCompletion,
) -> Result<Vec<Op>, SessionError> {
	let turn = session
		.dom()
		.children(session.dom().body())
		.last()
		.copied()
		.ok_or(SessionError::NoActiveTurn)?;
	let data =
		serde_json::value::to_raw_value(&LaunchCompletion { daemons: vec![completion.clone()] })?;
	let mut body = StrMut::new("");
	let _ = write!(body, "Supervised process {} {} ", completion.name, completion.status);
	match completion.exit_code {
		Some(code) => {
			let _ = write!(body, "with exit code {code}.");
		},
		None => body.push_str("without an exit code."),
	}
	if let Some(fault) = &completion.fault {
		let _ = write!(body, " Fault: {}", fault.kind);
		if let Some(message) = &fault.message {
			let _ = write!(body, " — {message}");
		}
		if let Some(signal) = &fault.signal {
			let _ = write!(body, " ({signal})");
		}
		body.push('.');
	}
	Ok(vec![
		Op::Ins {
			parent: turn,
			after:  session.dom().children(turn).last().copied(),
			node:   NodeSpec::new(KnownTag::User)
				.with_prop(PropKey::Custom(Str::new_static("launch_completion")), Value::Bool(true))
				.with_prop(PropId::Data, Value::Json(data))
				.with_content(body.freeze()),
		},
		Op::Set {
			h:     job,
			prop:  PropKey::Custom(Str::new_static(crate::jobs::DELIVERED)),
			value: Value::Bool(true),
		},
	])
}

/// Journals one typed IRC observation under the active turn.
///
/// The typed JSON remains the presentation contract across replay while the
/// duplicate body content keeps generic fallback and copy projections lossless.
pub fn append_irc_traffic(
	session: &mut Session,
	turn: Handle,
	payload: &IrcTraffic,
) -> Result<(), SessionError> {
	let data = serde_json::value::to_raw_value(payload)?;
	session.patch(omp_dom::Txn {
		cause: session.head().ok_or(SessionError::NoActiveTurn)?,
		label: Some(Str::new_static("kernel.irc")),
		ops:   vec![Op::Ins {
			parent: turn,
			after:  session.dom().children(turn).last().copied(),
			node:   NodeSpec::new(KnownTag::Notice)
				.with_prop(PropId::Kind, Value::Str(Str::new_static("irc")))
				.with_prop(PropId::Data, Value::Json(data))
				.with_content(payload.body.clone()),
		}],
	})?;
	Ok(())
}
