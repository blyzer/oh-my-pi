//! Operator workspace-trust commands over the local grant file.

use std::{
	io::{self, Write},
	path::Path,
};

use miette::{IntoDiagnostic as _, Result};
use omp_driver::workspace_trust::evaluate_workspace;
use omp_ext::{
	trust::{GrantDuration, GrantScope, GrantsFile, grants_path},
	workspace_trust::{
		CanonicalHome, TrustChannel, TrustDecision, WorkspaceTrustGrant, evaluate,
		inventory::{InventoryBudget, inventory},
	},
};

use crate::cli::TrustArgs;

/// Records, revokes, denies, or inspects workspace trust.
///
/// # Errors
///
/// Returns inventory, subtree refusal, grant persistence, and output failures.
pub fn run(args: &TrustArgs) -> Result<()> {
	let data = omp_core::dirs::data_dir(None).into_diagnostic()?;
	run_with(args, &data, &CanonicalHome::current(), &mut io::stdout().lock())
}

fn run_with(
	args: &TrustArgs,
	data: &Path,
	home: &CanonicalHome,
	out: &mut impl Write,
) -> Result<()> {
	let path = grants_path(data);
	if args.revoke {
		// Remove the subtree first so an exact revoke cannot leave a deny
		// solely because of the subtree being removed here.
		let subtree = GrantsFile::revoke_workspace_trust(
			&path,
			&args.path,
			GrantScope::Subtree,
			TrustChannel::Cli,
		)
		.into_diagnostic()?;
		let exact = GrantsFile::revoke_workspace_trust(
			&path,
			&args.path,
			GrantScope::Exact,
			TrustChannel::Cli,
		)
		.into_diagnostic()?;
		writeln!(out, "Revoked {}: {} trust rows removed.", args.path.display(), subtree + exact)
			.into_diagnostic()?;
		return Ok(());
	}
	let workspace = args.path.canonicalize().into_diagnostic()?;
	if args.show {
		let decision = evaluate_workspace(data, &workspace);
		writeln!(out, "Workspace {}: {}", workspace.display(), decision.kind()).into_diagnostic()?;
		match decision {
			TrustDecision::Trusted { duration } => writeln!(out, "Digest match: yes ({duration})"),
			TrustDecision::DigestChanged { granted, current } => {
				writeln!(out, "Digest match: no\nGranted: {granted}\nCurrent: {current}")
			},
			TrustDecision::PinUnderSubtree { root, duration } => writeln!(
				out,
				"Covering subtree: {} ({duration}); confirmation required",
				root.display()
			),
			TrustDecision::Denied | TrustDecision::Untrusted => Ok(()),
		}
		.into_diagnostic()?;
		return Ok(());
	}
	if args.subtree {
		let grant = WorkspaceTrustGrant::subtree(
			&workspace,
			home,
			TrustChannel::Cli,
			GrantDuration::Persistent,
		)
		.into_diagnostic()?;
		GrantsFile::persist_workspace_subtree(&path, grant)
			.into_diagnostic()?
			.into_diagnostic()?;
		writeln!(
			out,
			"Granted subtree {}: each workspace still requires a digest-bound confirmation.",
			workspace.display()
		)
		.into_diagnostic()?;
		return Ok(());
	}
	if args.deny {
		let grant =
			WorkspaceTrustGrant::deny(&workspace, TrustChannel::Cli, GrantDuration::Persistent)
				.into_diagnostic()?;
		GrantsFile::persist_workspace_deny(&path, grant)
			.into_diagnostic()?
			.into_diagnostic()?;
		writeln!(out, "Denied workspace {}.", workspace.display()).into_diagnostic()?;
		return Ok(());
	}
	let inputs = inventory(&workspace, data, &InventoryBudget::default()).into_diagnostic()?;
	let grants = GrantsFile::read(&path).into_diagnostic()?;
	let answered = evaluate(&grants.workspace_trust, &workspace, inputs.digest(), home);
	let grant = if let TrustDecision::PinUnderSubtree { root, .. } = &answered {
		WorkspaceTrustGrant::pin(
			&workspace,
			*inputs.digest(),
			root,
			TrustChannel::Cli,
			GrantDuration::Persistent,
		)
	} else {
		WorkspaceTrustGrant::exact(
			&workspace,
			*inputs.digest(),
			TrustChannel::Cli,
			GrantDuration::Persistent,
		)
	}
	.into_diagnostic()?;
	GrantsFile::persist_workspace_trust(&path, grant, &answered, home)
		.into_diagnostic()?
		.into_diagnostic()?;
	writeln!(
		out,
		"Trusted workspace {}: {} ({} present inputs).",
		workspace.display(),
		inputs.digest(),
		inputs.present().count()
	)
	.into_diagnostic()?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use std::{env, fs, path::PathBuf};

	use omp_ext::workspace_trust::{TrustBinding, WorkspaceTrustError};

	use super::*;

	struct Workspace {
		_scratch: tempfile::TempDir,
		data:     PathBuf,
		home:     CanonicalHome,
		project:  PathBuf,
	}

	impl Workspace {
		fn new() -> Self {
			let scratch = tempfile::tempdir().unwrap();
			let root = scratch.path().canonicalize().unwrap();
			let project = root.join("team/project");
			let home = root.join("home");
			let data = root.join("data");
			fs::create_dir_all(project.join(".omp")).unwrap();
			fs::create_dir(&home).unwrap();
			fs::write(
				project.join(".omp/mcp.json"),
				r#"{"mcpServers":{"local":{"command":"trust-test-server"}}}"#,
			)
			.unwrap();
			// SAFETY: nextest isolates tests by process; no concurrent environment readers.
			unsafe {
				env::set_var("HOME", &home);
				env::set_var("OMP_CONFIG_DIR", root.join("config"));
				env::set_var("OMP_DATA_DIR", &data);
				env::set_var("CLAUDE_CONFIG_DIR", root.join("claude"));
			}
			Self { _scratch: scratch, data, home: CanonicalHome::resolve(Some(&home)), project }
		}

		fn args(&self) -> TrustArgs {
			TrustArgs {
				path:    self.project.clone(),
				subtree: false,
				revoke:  false,
				deny:    false,
				show:    false,
			}
		}

		fn command(&self, args: &TrustArgs) -> String {
			let mut out = Vec::new();
			run_with(args, &self.data, &self.home, &mut out).unwrap();
			String::from_utf8(out).unwrap()
		}
	}

	#[test]
	fn exact_grant_binds_digest_and_show_is_read_only() {
		let w = Workspace::new();
		assert!(w.command(&w.args()).contains("sha256:"));
		let before = fs::read(grants_path(&w.data)).unwrap();
		let mut show = w.args();
		show.show = true;
		assert!(w.command(&show).contains("Digest match: yes (persistent)"));
		assert_eq!(before, fs::read(grants_path(&w.data)).unwrap());
		fs::write(w.project.join(".omp/mcp.json"), "{}").unwrap();
		let report = w.command(&show);
		assert!(report.contains("digest_changed"));
		assert!(report.contains("Digest match: no\nGranted: sha256:"));
		assert_eq!(before, fs::read(grants_path(&w.data)).unwrap());
	}

	#[test]
	fn show_untrusted_creates_no_grant_file() {
		let w = Workspace::new();
		let mut args = w.args();
		args.show = true;
		assert!(w.command(&args).contains("untrusted"));
		assert!(!grants_path(&w.data).exists());
	}

	#[test]
	fn revoke_withholds_and_deny_is_replaced_only_by_explicit_trust() {
		let w = Workspace::new();
		w.command(&w.args());
		let mut revoke = w.args();
		revoke.revoke = true;
		assert!(w.command(&revoke).contains("1 trust rows removed"));
		assert_eq!(evaluate_workspace(&w.data, &w.project), TrustDecision::Untrusted);
		let mut deny = w.args();
		deny.deny = true;
		w.command(&deny);
		assert_eq!(evaluate_workspace(&w.data, &w.project), TrustDecision::Denied);
		w.command(&w.args());
		assert_eq!(evaluate_workspace(&w.data, &w.project), TrustDecision::Trusted {
			duration: GrantDuration::Persistent,
		});
	}

	#[test]
	fn subtree_child_requires_pin_and_revoke_cannot_fall_back_to_parent() {
		let w = Workspace::new();
		let mut subtree = w.args();
		subtree.path = w.project.parent().unwrap().to_path_buf();
		subtree.subtree = true;
		w.command(&subtree);
		assert!(matches!(
			evaluate_workspace(&w.data, &w.project),
			TrustDecision::PinUnderSubtree { .. }
		));
		let mut show = w.args();
		show.show = true;
		assert!(w.command(&show).contains("Covering subtree:"));
		w.command(&w.args());
		let rows = GrantsFile::read(&grants_path(&w.data))
			.unwrap()
			.workspace_trust;
		assert!(
			rows
				.iter()
				.any(|row| matches!(row.binding, TrustBinding::Pin { .. }))
		);
		let mut revoke = w.args();
		revoke.revoke = true;
		w.command(&revoke);
		assert_eq!(evaluate_workspace(&w.data, &w.project), TrustDecision::Denied);
		subtree.subtree = false;
		subtree.revoke = true;
		w.command(&subtree);
		assert_eq!(
			GrantsFile::read(&grants_path(&w.data))
				.unwrap()
				.workspace_trust
				.len(),
			1
		);
	}

	#[test]
	fn subtree_home_and_root_refusal_are_typed_and_write_nothing() {
		let w = Workspace::new();
		let CanonicalHome::Resolved(home) = &w.home else {
			panic!("resolved home")
		};
		for path in [home.as_path(), Path::new("/")] {
			let mut args = w.args();
			args.path = path.to_path_buf();
			args.subtree = true;
			let error = run_with(&args, &w.data, &w.home, &mut Vec::new()).unwrap_err();
			// `into_diagnostic` erases the type; the typed variant is pinned in `omp-ext`.
			let refusal = WorkspaceTrustError::SubtreeTooBroad { root: path.to_path_buf() };
			assert_eq!(error.to_string(), refusal.to_string());
			assert!(!grants_path(&w.data).exists());
		}
	}
}
