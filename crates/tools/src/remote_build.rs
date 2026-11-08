//! Remote build contract: one remote-eligible `just` recipe of this
//! repository, run on the operator's remote executor as a background execution
//! job.
//!
//! The session that offers `remote_build` runs every call itself (ADR 0040,
//! Part A.3): it asks the user to approve the operator's endpoint host before
//! anything connects or uploads, and never falls back to a local run. This
//! module fixes the declaration and the typed outcome that session journals.

use std::sync::Arc;

use async_stream::stream;
use futures::Stream;
use omp_core::{Str, sf};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, DocEffects, Effects, Ev, ExecEffects,
	IncomingParams, ParamError, Part, PromptCaps, Rev, Tool, ToolSpec, ToolTerminal,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Model-facing name of the remote build tool.
pub const NAME: &str = "remote_build";

/// One remote build request.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
	/// The `just` recipe to run remotely, such as `test-pkg`, `check-pkg`,
	/// `clippy` or `e2e-core`.
	pub recipe:  Str,
	/// The workspace package `test-pkg` and `check-pkg` take, such as
	/// `omp-driver`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub package: Option<Str>,
}

/// The execution job one remote build started.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Payload {
	/// Durable id of the execution job, which settles in the background.
	pub job:         Str,
	/// The approved remote endpoint, without credentials.
	pub destination: Str,
}

/// Remote builds stream no progress.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Update {}

/// Why a remote build started nothing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fault {
	/// This session does not run remote builds.
	#[error("remote builds are not enabled in this session")]
	Unavailable,
	/// The recipe is not one the remote executor runs.
	#[error("`{recipe}` is not a remote-eligible just recipe")]
	Ineligible {
		/// The refused recipe.
		recipe: Str,
	},
	/// The package does not name a workspace package.
	#[error("`{package}` is not a workspace package name such as omp-driver")]
	InvalidPackage {
		/// The refused package.
		package: Str,
	},
	/// The operator has not configured a usable remote endpoint and image.
	#[error("remote execution is not configured")]
	Unconfigured,
	/// The user did not approve the upload to the endpoint host.
	#[error("the user did not approve a remote build on {host}")]
	NotApproved {
		/// The endpoint host the prompt named.
		host: Str,
	},
	/// The approval already started an earlier remote build; each approval
	/// starts one.
	#[error("approval {ticket} already started a remote build")]
	ApprovalSpent {
		/// The spent approval ticket.
		ticket: Str,
	},
	/// A tracked input matched an upload deny rule.
	#[error("`{path}` matched the upload deny rule `{rule}`")]
	InputRefused {
		/// The refused path.
		path: Str,
		/// The rule that refused it.
		rule: Str,
	},
	/// The build could not be planned, or its inputs could not be captured.
	#[error("the build inputs could not be captured")]
	Capture,
	/// The credential could not be resolved, or the endpoint could not be
	/// reached.
	#[error("the remote build could not connect to {host}")]
	Connect {
		/// The endpoint host.
		host: Str,
	},
	/// The execution job could not be recorded.
	#[error("the remote build job could not be recorded")]
	Journal,
}

/// Returns the `remote_build@1` declaration.
#[must_use]
pub fn spec() -> ToolSpec {
	ToolSpec {
		name:            Str::new_static(NAME),
		rev:             Rev { family: Str::default(), n: 1 },
		description:     sf!(
			"Runs one remote-eligible `just` recipe of this repository on the operator's remote \
			 executor, after the user approves uploading the working tree to its host, and returns \
			 the background execution job."
		),
		schema:          omp_tool::schema::<Params>(),
		constraint:      Constraint::Schema {
			priority:       100,
			on_unsupported: omp_tool::Fallback::Unspecified,
		},
		effects:         Effects {
			documents: Some(DocEffects { read: true, write_globs: Arc::from([]) }),
			exec: Some(ExecEffects { commands: Arc::from([Str::new_static("just")]), network: true }),
			..Effects::empty()
		},
		confinement:     omp_tool::Confinement::Host,
		projection_code: omp_tool::native_projection_code(
			env!("CARGO_PKG_NAME"),
			env!("CARGO_PKG_VERSION"),
			include_bytes!("remote_build.rs"),
		)
		.into(),
	}
}

/// The registry declaration of `remote_build@1`.
///
/// The session that offers the tool routes every call to its own executor
/// before the registry is consulted, so this value only refuses.
pub struct Declaration {
	spec: ToolSpec,
}

/// Creates the `remote_build@1` declaration.
#[must_use]
pub fn declaration() -> Declaration {
	Declaration { spec: spec() }
}

impl Tool for Declaration {
	type Fault = Fault;
	type Params = Params;
	type Payload = Payload;
	type Update = Update;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut incoming: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<Update, Payload, Fault>> + Send + 'c {
		stream! {
			if let Err(error) = incoming.whole::<Params>().await {
				yield param_event(error);
				return;
			}
			if let Err(error) = incoming.interruptable().committed().await {
				yield commit_event(error);
				return;
			}
			yield Ev::Done(ToolTerminal::Done { result: Err(Fault::Unavailable), useless: true });
		}
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		let text = match view {
			Ok(payload) => {
				sf!("Remote build job {} started on {}.", payload.job, payload.destination)
			},
			Err(fault) => sf!("{fault}"),
		};
		vec![Part::Text { text }]
	}
}

fn param_event(error: ParamError) -> Ev<Update, Payload, Fault> {
	match error {
		ParamError::Args(issue) => Ev::Args(*issue),
		ParamError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		ParamError::Protocol { violation: message } => Ev::Args(protocol_issue(message)),
	}
}

fn commit_event(error: CommitError) -> Ev<Update, Payload, Fault> {
	match error {
		CommitError::Aborted => Ev::Aborted(Abort::InputDropped),
		CommitError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		CommitError::Protocol { violation: message } => Ev::Args(protocol_issue(message)),
	}
}

fn protocol_issue(message: Str) -> ArgIssue {
	ArgIssue {
		path:     Vec::new(),
		expected: sf!("one committed JSON argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  Some(sf!(r#"{{"recipe":"test-pkg","package":"omp-driver"}}"#)),
		found:    Some(message),
	}
}

#[cfg(test)]
mod tests {
	use futures::StreamExt as _;
	use serde_json::{Value, json};

	use super::*;

	#[test]
	fn schema_takes_a_recipe_and_an_optional_package() {
		let spec = spec();
		assert_eq!(spec.name, "remote_build");
		assert_eq!(spec.rev.n, 1);
		let schema: Value = serde_json::from_slice(&spec.schema).expect("schema");
		assert_eq!(schema["required"], json!(["i", "recipe"]));
		assert_eq!(schema["additionalProperties"], json!(false));
		let mut properties = schema["properties"]
			.as_object()
			.expect("properties")
			.keys()
			.map(String::as_str)
			.collect::<Vec<_>>();
		properties.sort_unstable();
		assert_eq!(properties, ["i", "notrunc", "package", "recipe"]);
	}

	#[test]
	fn faults_are_tagged_by_kind() {
		let fault = Fault::NotApproved { host: Str::new_static("build.example") };
		assert_eq!(
			serde_json::to_value(&fault).expect("fault"),
			json!({ "kind": "not_approved", "host": "build.example" })
		);
		assert_eq!(fault.to_string(), "the user did not approve a remote build on build.example");
		assert_eq!(
			serde_json::to_value(Fault::ApprovalSpent { ticket: Str::new_static("approval-7") })
				.expect("fault"),
			json!({ "kind": "approval_spent", "ticket": "approval-7" })
		);
	}

	#[tokio::test]
	async fn the_declaration_alone_runs_nothing() {
		let raw = r#"{"recipe":"test-pkg","package":"omp-driver","i":"Testing remotely"}"#;
		let (feed, incoming) = IncomingParams::channel();
		feed.arg_text(raw.into()).expect("stream args");
		feed.args_committed(raw.into()).expect("commit args");
		let events = declaration().call(incoming).collect::<Vec<_>>().await;
		assert!(
			matches!(
				events.last(),
				Some(Ev::Done(ToolTerminal::Done { result: Err(Fault::Unavailable), .. }))
			),
			"{events:?}"
		);
	}
}
