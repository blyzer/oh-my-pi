//! Interactive trust preflight, before discovery or kernel composition.

use std::path::Path;

use miette::{IntoDiagnostic as _, miette};
use omp_chat::overlays::workspace_trust::{TrustChoice, TrustPrompt};
use omp_core::Str;
use omp_ext::{
	trust::{GrantDuration, GrantsFile, grants_path},
	workspace_trust::{
		CanonicalHome, TrustChannel, TrustDecision, WorkspaceTrustGrant, evaluate,
		inventory::{InputsInventory, InventoryBudget, inventory},
	},
};

fn prompt(
	project: &Path,
	decision: &TrustDecision,
	inputs: &InputsInventory,
) -> Option<TrustPrompt> {
	if !matches!(
		decision,
		TrustDecision::Untrusted
			| TrustDecision::DigestChanged { .. }
			| TrustDecision::PinUnderSubtree { .. }
	) || inputs.present().next().is_none()
	{
		return None;
	}
	Some(TrustPrompt {
		workspace: Str::new(project.to_string_lossy()),
		inputs:    inputs
			.present()
			.map(|entry| Str::new(String::from_utf8_lossy(entry.path())))
			.collect(),
		changed:   matches!(decision, TrustDecision::DigestChanged { .. }),
		subtree:   match decision {
			TrustDecision::PinUnderSubtree { root, .. } => Some(Str::new(root.to_string_lossy())),
			_ => None,
		},
	})
}

/// Resolves the first-run answer before launch can discover executable project
/// inputs.
#[expect(clippy::future_not_send, reason = "the startup selector owns a thread-confined TUI App")]
pub(super) async fn ask(project: &Path, data: &Path, home: &Path) -> miette::Result<TrustDecision> {
	let project = std::fs::canonicalize(project).into_diagnostic()?;
	let inputs = inventory(&project, data, &InventoryBudget::default()).into_diagnostic()?;
	let grants = GrantsFile::read(&grants_path(data)).into_diagnostic()?;
	let home = CanonicalHome::resolve(Some(home));
	let decision = evaluate(&grants.workspace_trust, &project, inputs.digest(), &home);
	let Some(prompt) = prompt(&project, &decision, &inputs) else {
		return Ok(decision);
	};
	let choice = prompt.run().await.into_diagnostic()?;
	apply(choice, &decision, &inputs, &project, data, &home)
}

fn apply(
	choice: TrustChoice,
	answered: &TrustDecision,
	inputs: &InputsInventory,
	project: &Path,
	data: &Path,
	home: &CanonicalHome,
) -> miette::Result<TrustDecision> {
	if choice == TrustChoice::Deny {
		return Ok(answered.clone());
	}
	// Approval binds the inventory displayed, never a replacement written while the
	// dialog was open.
	let current = inventory(project, data, &InventoryBudget::default()).into_diagnostic()?;
	if current.digest() != inputs.digest() {
		return Err(miette!("workspace inputs changed while awaiting trust; restart to review them"));
	}
	let grants = GrantsFile::read(&grants_path(data)).into_diagnostic()?;
	if evaluate(&grants.workspace_trust, project, inputs.digest(), home) != *answered {
		return Err(miette!("workspace grants changed while awaiting trust; restart to review them"));
	}
	let (grant, duration) = match choice {
		TrustChoice::Deny => unreachable!("denial returned before grant construction"),
		TrustChoice::Session => {
			return Ok(TrustDecision::Trusted { duration: GrantDuration::Session });
		},
		TrustChoice::Workspace => (
			WorkspaceTrustGrant::exact(
				project,
				*inputs.digest(),
				TrustChannel::Interactive,
				GrantDuration::Persistent,
			)
			.into_diagnostic()?,
			GrantDuration::Persistent,
		),
		TrustChoice::Pin => {
			let TrustDecision::PinUnderSubtree { root, duration } = answered else {
				return Err(miette!("no subtree grant is available to pin this workspace"));
			};
			(
				WorkspaceTrustGrant::pin(
					project,
					*inputs.digest(),
					root,
					TrustChannel::Interactive,
					*duration,
				)
				.into_diagnostic()?,
				*duration,
			)
		},
	};
	if duration == GrantDuration::Persistent {
		GrantsFile::persist_workspace_trust(&grants_path(data), grant, answered, home)
			.into_diagnostic()?
			.into_diagnostic()?;
	}
	Ok(TrustDecision::Trusted { duration })
}

#[cfg(test)]
mod tests {
	use omp_ext::workspace_trust::TrustBinding;

	use super::*;

	struct Fixture {
		root:    tempfile::TempDir,
		project: std::path::PathBuf,
		data:    std::path::PathBuf,
		home:    CanonicalHome,
	}
	impl Fixture {
		fn new() -> Self {
			let root = tempfile::tempdir().unwrap();
			// `ask` canonicalizes before evaluating; grants hold canonical paths.
			let base = std::fs::canonicalize(root.path()).unwrap();
			let project = base.join("team/project");
			let data = base.join("data");
			let home_path = base.join("home");
			std::fs::create_dir_all(project.join(".omp")).unwrap();
			std::fs::create_dir_all(&home_path).unwrap();
			std::fs::write(project.join(".omp/SYSTEM.md"), "trusted prompt").unwrap();
			Self { root, project, data, home: CanonicalHome::resolve(Some(&home_path)) }
		}

		fn inputs(&self) -> InputsInventory {
			inventory(&self.project, &self.data, &InventoryBudget::default()).unwrap()
		}

		fn answer(&self, choice: TrustChoice, decision: &TrustDecision) -> TrustDecision {
			apply(choice, decision, &self.inputs(), &self.project, &self.data, &self.home).unwrap()
		}
	}

	#[test]
	fn denial_and_session_do_not_persist() {
		let f = Fixture::new();
		assert_eq!(f.answer(TrustChoice::Deny, &TrustDecision::Untrusted), TrustDecision::Untrusted);
		assert_eq!(
			f.answer(TrustChoice::Session, &TrustDecision::Untrusted),
			TrustDecision::Trusted { duration: GrantDuration::Session }
		);
		assert!(!grants_path(&f.data).exists());
	}

	#[test]
	fn workspace_choice_persists_the_displayed_digest_and_journals() {
		let f = Fixture::new();
		let decision = f.answer(TrustChoice::Workspace, &TrustDecision::Untrusted);
		let grants = GrantsFile::read(&grants_path(&f.data)).unwrap();
		assert_eq!(grants.workspace_trust.len(), 1);
		assert_eq!(grants.workspace_trust[0].binding, TrustBinding::Exact {
			inputs_digest: *f.inputs().digest(),
		});
		assert_eq!(grants.workspace_trust[0].granted_by, TrustChannel::Interactive);
		assert_eq!(
			evaluate(
				&grants.workspace_trust,
				&std::fs::canonicalize(&f.project).unwrap(),
				f.inputs().digest(),
				&f.home
			),
			decision
		);
		let mut session = omp_session::Session::create(
			f.root.path().join("trust.oms"),
			omp_session::ComponentRegistry::standard(),
		)
		.unwrap();
		omp_driver::workspace_trust::journal_decision(&mut session, &decision).unwrap();
		assert_eq!(omp_driver::workspace_trust::journaled_decision(&session), Some(decision));
	}

	#[test]
	fn subtree_choice_persists_a_pin_not_a_broader_grant() {
		let f = Fixture::new();
		let team = f.project.parent().unwrap();
		let subtree =
			WorkspaceTrustGrant::subtree(team, &f.home, TrustChannel::Cli, GrantDuration::Persistent)
				.unwrap();
		GrantsFile::persist_workspace_subtree(&grants_path(&f.data), subtree)
			.unwrap()
			.unwrap();
		let decision = TrustDecision::PinUnderSubtree {
			root:     std::fs::canonicalize(team).unwrap(),
			duration: GrantDuration::Persistent,
		};
		assert_eq!(f.answer(TrustChoice::Pin, &decision), TrustDecision::Trusted {
			duration: GrantDuration::Persistent,
		});
		let grants = GrantsFile::read(&grants_path(&f.data)).unwrap();
		assert!(
			grants
				.workspace_trust
				.iter()
				.any(|grant| matches!(grant.binding, TrustBinding::Pin { .. }))
		);
	}

	#[test]
	fn trusted_denied_and_empty_inventory_never_prompt() {
		let f = Fixture::new();
		for decision in
			[TrustDecision::Trusted { duration: GrantDuration::Persistent }, TrustDecision::Denied]
		{
			assert!(prompt(&f.project, &decision, &f.inputs()).is_none());
		}
		assert!(prompt(&f.project, &TrustDecision::Untrusted, &f.inputs()).is_some());
		std::fs::remove_file(f.project.join(".omp/SYSTEM.md")).unwrap();
		assert!(prompt(&f.project, &TrustDecision::Untrusted, &f.inputs()).is_none());
	}

	#[test]
	fn digest_change_reasks_and_stale_input_cannot_be_approved() {
		let f = Fixture::new();
		let old = f.inputs();
		f.answer(TrustChoice::Workspace, &TrustDecision::Untrusted);
		std::fs::write(f.project.join(".omp/SYSTEM.md"), "changed prompt").unwrap();
		let changed =
			TrustDecision::DigestChanged { granted: *old.digest(), current: *f.inputs().digest() };
		assert!(prompt(&f.project, &changed, &f.inputs()).unwrap().changed);
		assert!(
			apply(
				TrustChoice::Workspace,
				&TrustDecision::Untrusted,
				&old,
				&f.project,
				&f.data,
				&f.home
			)
			.is_err()
		);
		assert_eq!(f.answer(TrustChoice::Workspace, &changed), TrustDecision::Trusted {
			duration: GrantDuration::Persistent,
		});
	}
}
