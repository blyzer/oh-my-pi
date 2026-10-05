//! Plan-mode write confinement at the environment's writers.
//!
//! The agent refuses an off-plan call at dispatch, with the model-visible
//! roster message. This module is the environment's safety net behind it: an
//! invocation whose roster restrictions (`InvokeTool.restrictions`) carry a
//! plan file (plan mode is active) or a read-only ceiling (the caller is a
//! subagent of a plan-mode session) runs inside a [`WriteScope`], and every
//! writer the environment owns refuses, with a typed [`WriteScopeDenied`],
//! any change the scope does not admit:
//!
//! - the document authority client ([`crate::docs::DocumentHost`]): text,
//!   create, delete, and move transactions, and directory, removal, rename,
//!   copy, link, and permission requests;
//! - the tool document host's own direct writers: plain writes outside the
//!   workspace (the `local://` scratch root included), archive members, SQLite
//!   rows, and SSH, vault, and RPC-host resource writes.
//!
//! The scope is per invocation and derived from that invocation's snapshot,
//! so it ends with the call and never outlives plan mode. Nested
//! `tool.<name>()` calls of an eval cell run in the scope of the `eval`
//! invocation that started the cell. Tools whose writes bypass these writers
//! (processes, direct filesystem writers) are refused at the invocation
//! boundary instead (`InvocationExecutionPolicy` in the server).

use std::{
	future::Future,
	path::{Component, Path, PathBuf},
	sync::Arc,
};

use omp_core::Str;
use omp_tool::{ToolRestrictions, plan_target_matches};
use omp_tools::path::local_resource;
use url::Url;

use crate::tool_url::local::{canonicalize_allowing_missing, local_path, principal_local_root};

tokio::task_local! {
	static WRITE_SCOPE: Option<Arc<WriteScope>>;
}

/// The changes one invocation may make.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WriteScope {
	/// Plan mode is active: only the plan file may change.
	PlanFile {
		/// The plan file as the plan Director names it.
		plan_file: Str,
		/// The path it names for this invocation; `None` when it names none
		/// (a `local://` file without a session principal, a traversing
		/// spelling, a non-file resource), so only an equal resource URI may
		/// change.
		target:    Option<PathBuf>,
	},
	/// The caller is a subagent of a plan-mode session: nothing may change.
	ReadOnly,
}

/// A write the invocation's scope does not admit (the contract lives in
/// `omp-tool`, so a write tool can journal it as its typed fault).
pub use omp_tool::WriteScopeDenied;

impl WriteScope {
	/// The scope an invocation with `restrictions` runs in, if any. A
	/// `local://` plan file resolves inside the scratch root of the session
	/// `principal` names; a relative one against `workspace_root`.
	pub(crate) fn for_invocation(
		restrictions: &ToolRestrictions,
		workspace_root: &Path,
		sessions_dir: &Path,
		principal: Option<&str>,
	) -> Option<Self> {
		if restrictions.read_only_ceiling().is_some() {
			return Some(Self::ReadOnly);
		}
		let plan_file = restrictions.plan_file()?;
		Some(Self::PlanFile {
			plan_file: plan_file.clone(),
			target:    plan_file_path(plan_file, workspace_root, sessions_dir, principal),
		})
	}

	/// Admits a change to the absolute `path`. The plan file matches by the
	/// path both resolve to, so a symlinked spelling of it is admitted and a
	/// symlink leading elsewhere is not.
	pub(crate) fn admit_path(&self, path: &Path) -> Result<(), WriteScopeDenied> {
		match self {
			Self::PlanFile { target: Some(plan), .. } if same_file(plan, path) => Ok(()),
			_ => Err(self.refuse(path.to_string_lossy())),
		}
	}

	/// Admits a change to `uri`: a `file:` URI by its path, any other
	/// resource only when it is the plan file's own spelling.
	pub(crate) fn admit_uri(&self, uri: &str) -> Result<(), WriteScopeDenied> {
		if let Some(path) = Url::parse(uri)
			.ok()
			.filter(|url| url.scheme() == "file")
			.and_then(|url| url.to_file_path().ok())
		{
			return self.admit_path(&path);
		}
		match self {
			Self::PlanFile { plan_file, .. } if plan_target_matches(plan_file, uri) => Ok(()),
			_ => Err(self.refuse(uri)),
		}
	}

	/// The refusal of a change to `target`.
	pub(crate) fn refuse(&self, target: impl Into<String>) -> WriteScopeDenied {
		let target = Str::from(target.into());
		match self {
			Self::PlanFile { plan_file, .. } => {
				WriteScopeDenied::OutsidePlanFile { plan_file: plan_file.clone(), target }
			},
			Self::ReadOnly => WriteScopeDenied::ReadOnly { target },
		}
	}
}

/// The scope of the invocation the current task runs, if any.
pub(crate) fn current() -> Option<Arc<WriteScope>> {
	WRITE_SCOPE.try_with(Clone::clone).ok().flatten()
}

/// Runs `future` inside `scope`; `None` runs it unscoped. One wrapper either
/// way: the invocation futures it wraps are large, and a second state
/// machine around them overflows a worker stack in debug builds.
pub(crate) fn scoped<F: Future>(
	scope: Option<Arc<WriteScope>>,
	future: F,
) -> tokio::task::futures::TaskLocalFuture<Option<Arc<WriteScope>>, F> {
	WRITE_SCOPE.scope(scope, future)
}

/// Admits a change to the absolute `path` under the current task's scope.
pub(crate) fn admit_path(path: &Path) -> Result<(), WriteScopeDenied> {
	current().map_or(Ok(()), |scope| scope.admit_path(path))
}

/// Admits a change to `uri` under the current task's scope.
pub(crate) fn admit_uri(uri: &str) -> Result<(), WriteScopeDenied> {
	current().map_or(Ok(()), |scope| scope.admit_uri(uri))
}

/// Admits a change the writer cannot attribute to a path (a lease- or
/// id-addressed document): any scope refuses it.
pub(crate) fn admit_unresolved(target: &str) -> Result<(), WriteScopeDenied> {
	current().map_or(Ok(()), |scope| Err(scope.refuse(target)))
}

fn plan_file_path(
	plan_file: &str,
	workspace_root: &Path,
	sessions_dir: &Path,
	principal: Option<&str>,
) -> Option<PathBuf> {
	let plan_file = plan_file.trim();
	if let Some(resource) = local_resource(plan_file) {
		let root = principal_local_root(sessions_dir, principal?).ok()?;
		return local_path(&root, resource).ok();
	}
	if let Ok(url) = Url::parse(plan_file) {
		return (url.scheme() == "file")
			.then(|| url.to_file_path().ok())
			.flatten();
	}
	let path = Path::new(plan_file);
	if path
		.components()
		.any(|component| matches!(component, Component::ParentDir))
	{
		return None;
	}
	Some(if path.is_absolute() {
		path.to_path_buf()
	} else {
		workspace_root.join(path)
	})
}

fn same_file(plan: &Path, path: &Path) -> bool {
	match (canonicalize_allowing_missing(plan), canonicalize_allowing_missing(path)) {
		(Ok(plan), Ok(path)) => plan == path,
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use omp_core::sf;

	use super::*;

	fn plan(plan_file: &'static str) -> ToolRestrictions {
		ToolRestrictions::default().with_plan_file(Str::new_static(plan_file))
	}

	#[test]
	fn restrictions_without_plan_file_or_ceiling_have_no_scope() {
		let restrictions = ToolRestrictions::default().with_allowlist(Arc::from([sf!("read")]), None);
		assert_eq!(
			WriteScope::for_invocation(&restrictions, Path::new("/w"), Path::new("/s"), None),
			None
		);
	}

	#[test]
	fn plan_scope_admits_only_the_plan_file() {
		let directory = tempfile::tempdir().expect("scratch");
		let workspace = directory.path().join("workspace");
		let sessions = directory.path().join("sessions");
		fs::create_dir_all(workspace.join("docs")).expect("workspace");
		let scope = WriteScope::for_invocation(
			&plan("local://PLAN.md"),
			&workspace,
			&sessions,
			Some("session-1"),
		)
		.expect("plan scope");
		let plan_path = sessions.join("session-1/local/PLAN.md");
		assert!(scope.admit_path(&plan_path).is_ok());
		let denial = scope
			.admit_path(&workspace.join("src/lib.rs"))
			.expect_err("off-plan write");
		assert!(matches!(denial, WriteScopeDenied::OutsidePlanFile { .. }));
		assert!(
			denial
				.to_string()
				.starts_with("plan mode is active: the environment refused")
		);
		assert!(
			scope
				.admit_path(&sessions.join("session-1/local/notes.md"))
				.is_err()
		);
		assert!(scope.admit_uri("vault://plans/PLAN.md").is_err());
		let file_uri = Url::from_file_path(&plan_path).expect("file URI");
		assert!(scope.admit_uri(file_uri.as_str()).is_ok());

		// A workspace plan file matches through a symlinked spelling only when
		// that spelling resolves to it.
		let scope = WriteScope::for_invocation(&plan("docs/PLAN.md"), &workspace, &sessions, None)
			.expect("plan scope");
		std::os::unix::fs::symlink(workspace.join("docs"), workspace.join("alias")).expect("symlink");
		assert!(scope.admit_path(&workspace.join("docs/PLAN.md")).is_ok());
		assert!(scope.admit_path(&workspace.join("alias/PLAN.md")).is_ok());
		assert!(scope.admit_path(&workspace.join("docs/OTHER.md")).is_err());

		// Without a principal a `local://` plan file names nothing writable.
		let scope = WriteScope::for_invocation(&plan("local://PLAN.md"), &workspace, &sessions, None)
			.expect("plan scope");
		assert!(scope.admit_path(&plan_path).is_err());
		let scope = WriteScope::for_invocation(&plan("../PLAN.md"), &workspace, &sessions, None)
			.expect("plan scope");
		assert!(scope.admit_path(&directory.path().join("PLAN.md")).is_err());
	}

	#[test]
	fn the_read_only_ceiling_admits_nothing() {
		let restrictions = plan("local://PLAN.md").with_read_only_ceiling(Arc::from([sf!("read")]));
		let scope = WriteScope::for_invocation(
			&restrictions,
			Path::new("/w"),
			Path::new("/s"),
			Some("session-1"),
		)
		.expect("read-only scope");
		assert_eq!(scope, WriteScope::ReadOnly);
		let denial = scope
			.admit_path(Path::new("/s/session-1/local/PLAN.md"))
			.expect_err("read-only");
		assert_eq!(
			denial.to_string(),
			"this agent is a read-only subagent of a plan-mode session: the environment refused to \
			 change /s/session-1/local/PLAN.md"
		);
		assert!(scope.admit_uri("vault://x").is_err());
	}

	#[tokio::test]
	async fn the_scope_is_task_local_and_ends_with_the_invocation() {
		let scope = Arc::new(WriteScope::ReadOnly);
		assert!(admit_path(Path::new("/w/a")).is_ok(), "no scope outside an invocation");
		scoped(Some(Arc::clone(&scope)), async {
			assert!(admit_path(Path::new("/w/a")).is_err());
			assert!(admit_unresolved("lease").is_err());
			assert!(
				tokio::spawn(async { admit_path(Path::new("/w/a")) })
					.await
					.expect("task")
					.is_ok(),
				"a spawned task carries no scope unless it is entered again"
			);
		})
		.await;
		assert!(admit_path(Path::new("/w/a")).is_ok(), "the scope ends with the invocation");
		scoped(None, async { assert!(admit_unresolved("lease").is_ok()) }).await;
	}
}
