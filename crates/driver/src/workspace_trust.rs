//! The workspace trust decision a composition runs under.
//!
//! The decision is a host-supplied composition input
//! ([`KernelOptions::workspace_trust`](crate::headless::KernelOptions::workspace_trust)),
//! never a convar. By default a composition evaluates it: it digests the
//! workspace's gated inputs ([`inventory`]) and decides them against the
//! operator's `[[workspace_trust]]` rows in the local grant file
//! ([`GrantsFile`]). Any failure on that path fails closed:
//! [`TrustDecision::Untrusted`]. A child composition (a subagent, a workflow
//! phase) inherits its parent's decision instead of evaluating its own
//! isolated root.
//!
//! The decision is journaled once per session on `<meta>`
//! ([`journal_decision`]): a session that already records the same decision
//! records nothing again.
//!
//! A composition that is not trusted tells the operator once what it withheld
//! ([`WithheldInputs`]), a typed [`WITHHELD_NOTICE`] notice: nothing when
//! the workspace holds no gated input.

use std::{fs, path::Path};

use omp_core::{Str, dirs};
use omp_dom::{Op, PropKey, Txn, Value};
use omp_ext::{
	trust::{GrantsFile, grants_path},
	workspace_trust::{
		CanonicalHome, TrustDecision, TrustDecisionKind, WorkspaceTrust, evaluate,
		inventory::{InventoryBudget, inventory},
	},
};
use omp_session::{Session, SessionError};
use serde::{Deserialize, Serialize};

/// `<meta>` property holding the session's journaled [`TrustDecision`].
pub const WORKSPACE_TRUST_PROP: &str = "workspace-trust";

/// Name of the typed notice reporting the inputs an untrusted workspace
/// withholds ([`WithheldInputs`]).
pub const WITHHELD_NOTICE: &str = "workspace-trust-withheld";

/// How many withheld inputs a [`WithheldInputs`] names; the rest are counted.
const NAMED_INPUTS: usize = 8;

/// Where a composition's [`TrustDecision`] comes from.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum WorkspaceTrustSource {
	/// Evaluate the workspace's inputs against the operator's grant file
	/// ([`evaluate_workspace`]).
	#[default]
	Evaluate,
	/// A decision the host already holds: a launch that evaluated it at its
	/// command boundary, a parent session's decision handed to a child, or a
	/// test's fixed decision.
	Fixed(TrustDecision),
}

impl WorkspaceTrustSource {
	/// The decision for the workspace at `project_root`, evaluating it with
	/// the grant file and plugin cache under `data_dir` when not fixed.
	#[must_use]
	pub fn decide(&self, data_dir: &Path, project_root: &Path) -> TrustDecision {
		match self {
			Self::Evaluate => evaluate_workspace(data_dir, project_root),
			Self::Fixed(decision) => decision.clone(),
		}
	}
}

/// Decides the trust of the workspace at `project_root`: its gated inputs'
/// digest against the durable `[[workspace_trust]]` rows of the grant file
/// under `data_dir`, keyed by the canonical root.
///
/// Fails closed: a root that cannot be canonicalized, an inventory refusal or
/// budget overrun, and an unreadable grant file all decide
/// [`TrustDecision::Untrusted`], each logged once.
#[must_use]
pub fn evaluate_workspace(data_dir: &Path, project_root: &Path) -> TrustDecision {
	let trust_key = match fs::canonicalize(project_root) {
		Ok(key) => key,
		Err(error) => {
			tracing::warn!(
				root = %project_root.display(),
				%error,
				"workspace root cannot be canonicalized; its inputs are untrusted"
			);
			return TrustDecision::Untrusted;
		},
	};
	let inputs = match inventory(&trust_key, data_dir, &InventoryBudget::default()) {
		Ok(inputs) => inputs,
		Err(error) => {
			tracing::warn!(
				root = %trust_key.display(),
				error = &error as &(dyn std::error::Error + 'static),
				"workspace inputs could not be inventoried; they are untrusted"
			);
			return TrustDecision::Untrusted;
		},
	};
	let grants = match GrantsFile::read(&grants_path(data_dir)) {
		Ok(grants) => grants,
		Err(error) => {
			tracing::warn!(
				error = &error as &(dyn std::error::Error + 'static),
				"the grant file could not be read; workspace inputs are untrusted"
			);
			return TrustDecision::Untrusted;
		},
	};
	let home = CanonicalHome::resolve(dirs::home_dir().as_deref());
	evaluate(&grants.workspace_trust, &trust_key, inputs.digest(), &home)
}

/// The decision journaled on `session`'s `<meta>`, if any.
#[must_use]
pub fn journaled_decision(session: &Session) -> Option<TrustDecision> {
	let dom = session.dom();
	match dom
		.get(dom.meta())?
		.prop(&PropKey::Custom(Str::new_static(WORKSPACE_TRUST_PROP)))?
	{
		Value::Json(raw) => serde_json::from_str(raw.get()).ok(),
		_ => None,
	}
}

/// Journals `decision` on `session`'s `<meta>` unless the session already
/// records it; returns whether it was journaled now.
///
/// # Errors
///
/// [`SessionError`] when the session has no head or the patch fails.
pub fn journal_decision(
	session: &mut Session,
	decision: &TrustDecision,
) -> Result<bool, SessionError> {
	if journaled_decision(session).as_ref() == Some(decision) {
		return Ok(false);
	}
	let raw = serde_json::value::to_raw_value(decision)?;
	session.patch(Txn {
		cause: session.head().ok_or(SessionError::NoActiveTurn)?,
		label: Some(Str::new_static("session.workspace-trust")),
		ops:   vec![Op::Set {
			h:     session.dom().meta(),
			prop:  PropKey::Custom(Str::new_static(WORKSPACE_TRUST_PROP)),
			value: Value::Json(raw),
		}],
	})?;
	Ok(true)
}

/// The gated inputs a workspace that is not trusted withholds, reported once
/// to the operator as a typed [`WITHHELD_NOTICE`] notice.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WithheldInputs {
	/// The decision that withholds them.
	pub decision: TrustDecisionKind,
	/// How many gated inputs are present.
	pub count:    usize,
	/// The first of them, as paths relative to the workspace root.
	pub named:    Vec<Str>,
}

impl WithheldInputs {
	/// What `decision` withholds of the workspace at `project_root`; `None`
	/// when it trusts the workspace or the workspace holds no gated input.
	/// A workspace whose inputs cannot be inventoried reports nothing here:
	/// [`evaluate_workspace`] already logged why it failed closed.
	#[must_use]
	pub fn of(decision: &TrustDecision, data_dir: &Path, project_root: &Path) -> Option<Self> {
		if decision.trust() == WorkspaceTrust::Trusted {
			return None;
		}
		let inputs = inventory(project_root, data_dir, &InventoryBudget::default())
			.inspect_err(|error| {
				tracing::debug!(
					error = error as &(dyn std::error::Error + 'static),
					"withheld workspace inputs could not be listed"
				);
			})
			.ok()?;
		let mut count = 0;
		let mut named = Vec::new();
		for entry in inputs.present() {
			count += 1;
			if named.len() < NAMED_INPUTS {
				named.push(Str::new(String::from_utf8_lossy(entry.path())));
			}
		}
		(count > 0).then(|| Self { decision: decision.kind(), count, named })
	}

	/// The one-line notice body.
	#[must_use]
	pub fn body(&self) -> Str {
		let mut listed = self.named.join(", ");
		if let Some(more) = self
			.count
			.checked_sub(self.named.len())
			.filter(|more| *more > 0)
		{
			listed.push_str(", and ");
			listed.push_str(&more.to_string());
			listed.push_str(" more");
		}
		let noun = if self.count == 1 { "input" } else { "inputs" };
		Str::new(format!(
			"Workspace {}: {} project {noun} withheld ({listed}) until this workspace is trusted.",
			self.decision, self.count
		))
	}

	/// Whether the live branch of `session` already holds this report.
	#[must_use]
	pub fn told(&self, session: &Session) -> bool {
		let dom = session.dom();
		let name = PropKey::Custom(Str::new_static("name"));
		let data = PropKey::from(omp_dom::PropId::Data);
		dom.handles()
			.filter_map(|handle| dom.get(handle))
			.any(|node| {
				node.tag == omp_dom::Tag::Known(omp_dom::KnownTag::Notice)
					&& node.prop(&name).and_then(Value::as_str) == Some(WITHHELD_NOTICE)
					&& matches!(node.prop(&data), Some(Value::Json(payload))
					if serde_json::from_str::<Self>(payload.get()).is_ok_and(|told| told == *self))
			})
	}
}

#[cfg(test)]
mod tests {
	use omp_session::ComponentRegistry;

	use super::*;

	#[test]
	fn the_decision_is_journaled_once_and_read_back() {
		let scratch = tempfile::tempdir().expect("scratch");
		let mut session =
			Session::create(scratch.path().join("trust.oms"), ComponentRegistry::standard())
				.expect("session");
		assert_eq!(journaled_decision(&session), None);
		assert!(journal_decision(&mut session, &TrustDecision::Untrusted).expect("journal"));
		assert!(!journal_decision(&mut session, &TrustDecision::Untrusted).expect("again"));
		assert_eq!(journaled_decision(&session), Some(TrustDecision::Untrusted));
		// A changed decision (the operator trusted the workspace since) is
		// recorded; the journal keeps both.
		assert!(journal_decision(&mut session, &TrustDecision::Denied).expect("changed"));
		assert_eq!(journaled_decision(&session), Some(TrustDecision::Denied));
	}

	#[test]
	fn an_unreadable_grant_file_or_a_missing_root_fails_closed() {
		let scratch = tempfile::tempdir().expect("scratch");
		let data = scratch.path().join("data");
		let project = scratch.path().join("project");
		fs::create_dir_all(&project).expect("project");
		assert_eq!(evaluate_workspace(&data, &project), TrustDecision::Untrusted);
		fs::create_dir_all(grants_path(&data)).expect("a directory in place of grants.toml");
		assert_eq!(evaluate_workspace(&data, &project), TrustDecision::Untrusted);
		assert_eq!(
			evaluate_workspace(&data, &scratch.path().join("missing")),
			TrustDecision::Untrusted
		);
	}
}
