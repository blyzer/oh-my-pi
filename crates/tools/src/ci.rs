//! Neutral CI tool (`ci@1`).
//!
//! `watch` defaults to false and reads status once. `watch: true` polls
//! status until the pipeline is terminal, then reads a log page for each
//! failed job. The interval is fixed. The old Actions fast-window, no-run,
//! and rate-limit policy stays inside the GitHub service and is not used here.

use std::{sync::Arc, time::Duration};

use async_stream::stream;
use futures::Stream;
use omp_ci::{
	CiError, CiHost, CiOp, CiProvider, CiStatus, Pipeline, RunRef, Trigger, jobs_with_status,
};
use omp_core::{Str, sf};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, Effects, Ev, IncomingParams, ParamError,
	Part, PromptCaps, RecordedCall, Rev, Tool, ToolSpec, ToolTerminal,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One trigger input.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
	/// Input name.
	pub key:   Str,
	/// Input value.
	pub value: Str,
}

/// Arguments for one `ci@1` call.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Params {
	/// Contract operation.
	pub op:       CiOp,
	/// Provider id. An omitted provider is a parameter error.
	pub provider: Option<Str>,
	/// Pipeline id, or the definition name for `trigger`.
	pub pipeline: Option<Str>,
	/// Job id, when the operation names one job.
	pub job:      Option<Str>,
	/// Poll status until the pipeline is terminal, then read failed-job logs.
	///
	/// Default is false. Only [`CiOp::Status`] watches. Other operations run
	/// once.
	#[serde(default)]
	pub watch:    bool,
	/// Trigger inputs. Empty when the caller passes none.
	#[serde(default)]
	pub inputs:   Vec<Input>,
}

/// Durable CI result.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Payload {
	/// Operation that completed.
	pub op:      CiOp,
	/// Contract value as JSON.
	pub result:  Value,
	/// Human projection.
	pub output:  Str,
	/// Whether the operation produced nothing to act on.
	pub useless: bool,
}

/// Model-facing CI failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct Fault {
	/// Stable category.
	pub code:    Str,
	/// Rendered diagnostic.
	pub message: Str,
}

/// One status observation while `watch` is set.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Update {
	/// Current human projection.
	pub output: Str,
}

/// CI tool bound to one host router.
pub struct Ci<H> {
	host: H,
	poll: Duration,
	spec: ToolSpec,
}

/// Creates `ci@1`.
///
/// Status polls every two seconds when `watch` is true.
pub fn tool<H: CiHost>(host: H) -> Ci<H> {
	Ci {
		host,
		poll: Duration::from_secs(2),
		spec: ToolSpec {
			name:            sf!("ci"),
			rev:             Rev { family: Str::default(), n: 1 },
			description:     sf!(
				"Reads and writes a CI provider: pipeline status, logs, artifacts, cancel, rerun, \
				 trigger, and play-manual. `provider` and `pipeline` are required. `watch` defaults \
				 to false; true polls status until the pipeline finishes and then reads logs for \
				 failed jobs. Cancel, rerun, trigger, and play-manual mutate."
			),
			schema:          omp_tool::schema::<Params>(),
			constraint:      Constraint::Schema {
				priority:       100,
				on_unsupported: omp_tool::Fallback::Unspecified,
			},
			effects:         Effects {
				documents: None,
				exec:      Some(omp_tool::ExecEffects { commands: Arc::from([]), network: true }),
				inference: None,
				desktop:   None,
				fetch:     None,
				subagents: 0,
			},
			confinement:     omp_tool::Confinement::Host,
			projection_code: omp_tool::native_projection_code(
				env!("CARGO_PKG_NAME"),
				env!("CARGO_PKG_VERSION"),
				include_bytes!("ci.rs"),
			)
			.into(),
		},
	}
}

impl<H: CiHost> Ci<H> {
	/// Sets how long `watch` waits between status reads.
	#[must_use]
	pub const fn with_poll(mut self, poll: Duration) -> Self {
		self.poll = poll;
		self
	}
}

impl<H: CiHost> Tool for Ci<H> {
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
			let params = match incoming.whole::<Params>().await {
				Ok(params) => params,
				Err(error) => {
					yield param_event(error);
					return;
				},
			};
			if let Err(error) = incoming.interruptable().committed().await {
				yield commit_event(error);
				return;
			}
			let (updates, update_rx) = flume::bounded(16);
			let execution = dispatch(&self.host, params, self.poll, &updates);
			tokio::pin!(execution);
			loop {
				tokio::select! {
					biased;
					result = &mut execution => {
						let useless = result.as_ref().is_ok_and(|payload| payload.useless);
						yield Ev::Done(ToolTerminal::Done { result, useless });
						break;
					},
					update = update_rx.recv_async() => {
						if let Ok(update) = update {
							yield Ev::Update(update);
						}
					},
					interrupt = incoming.next_interrupt() => {
						if let Ok(interrupt) = interrupt {
							yield Ev::Aborted(Abort::Interrupted { reason: interrupt.reason });
						} else {
							yield Ev::Aborted(Abort::InputDropped);
						}
						break;
					},
				}
			}
		}
	}

	fn prompt(&self, view: Result<&Payload, &Fault>, _: &PromptCaps) -> Vec<Part> {
		let text = match view {
			Ok(payload) => payload.output.clone(),
			Err(fault) => fault.message.clone(),
		};
		vec![Part::Text { text }]
	}

	fn lift(&self, _: &Rev, _: RecordedCall<'_>) -> Option<omp_tool::LiftedCall> {
		None
	}
}

async fn dispatch<H: CiHost>(
	host: &H,
	params: Params,
	poll: Duration,
	updates: &flume::Sender<Update>,
) -> Result<Payload, Fault> {
	let op = params.op;
	if op == CiOp::Trigger {
		let provider = provider(&params)?;
		let pipeline = required(params.pipeline.as_deref(), "ci trigger requires `pipeline`")?;
		let inputs = params
			.inputs
			.into_iter()
			.map(|input| (input.key, input.value));
		let request = Trigger::try_new(provider, pipeline, inputs).map_err(host_fault)?;
		let value = host.trigger(request).await.map_err(host_fault)?;
		return done(op, &value, render_pipeline(&value), false);
	}
	let run = run_ref(&params)?;
	if op == CiOp::Status && params.watch {
		return watch(host, run, poll, updates).await;
	}
	match op {
		CiOp::Status => {
			let value = host.status(&run).await.map_err(host_fault)?;
			done(op, &value, render_pipeline(&value), false)
		},
		CiOp::Artifacts => {
			let value = host.artifacts(&run).await.map_err(host_fault)?;
			let useless = value.artifacts.is_empty();
			done(op, &value, format!("{} artifacts", value.artifacts.len()), useless)
		},
		CiOp::LogStream => {
			let value = host.log_stream(&run).await.map_err(host_fault)?;
			done(op, &value, value.text.as_str().to_owned(), value.text.is_empty())
		},
		CiOp::Cancel => {
			let value = host.cancel(&run).await.map_err(host_fault)?;
			done(op, &value, render_pipeline(&value), false)
		},
		CiOp::RerunFailed => {
			let value = host.rerun_failed(&run).await.map_err(host_fault)?;
			done(op, &value, render_pipeline(&value), false)
		},
		CiOp::RerunJob => {
			let value = host.rerun_job(&run).await.map_err(host_fault)?;
			done(op, &value, format!("{} {}", value.status, value.name), false)
		},
		CiOp::PlayManual => {
			let value = host.play_manual(&run).await.map_err(host_fault)?;
			done(op, &value, format!("{} {}", value.status, value.name), false)
		},
		CiOp::Trigger => unreachable!("trigger is dispatched above"),
	}
}

async fn watch<H: CiHost>(
	host: &H,
	run: RunRef,
	poll: Duration,
	updates: &flume::Sender<Update>,
) -> Result<Payload, Fault> {
	loop {
		let pipeline = host.status(&run).await.map_err(host_fault)?;
		let mut output = render_pipeline(&pipeline);
		let _ = updates.try_send(Update { output: Str::new(output.clone()) });
		if !pipeline.status.is_terminal() {
			tokio::time::sleep(poll).await;
			continue;
		}
		if pipeline.status == CiStatus::Failure {
			for job in jobs_with_status(&pipeline, CiStatus::Failure) {
				let mut scoped = run.clone();
				scoped.job = Some(job.id.clone());
				if let Ok(page) = host.log_stream(&scoped).await
					&& !page.text.is_empty()
				{
					output.push('\n');
					output.push_str(page.text.as_str());
				}
			}
		}
		return done(CiOp::Status, &pipeline, output, false);
	}
}

fn run_ref(params: &Params) -> Result<RunRef, Fault> {
	let provider = provider(params)?;
	let pipeline = required(params.pipeline.as_deref(), "ci requires `pipeline`")?;
	RunRef::try_new(provider, pipeline, params.job.clone())
		.map_err(|error| host_fault(CiError::Id(error)))
}

fn provider(params: &Params) -> Result<CiProvider, Fault> {
	let provider = required(params.provider.as_deref(), "ci requires `provider`")?;
	CiProvider::try_new(provider).map_err(|error| host_fault(CiError::Id(error)))
}

fn required(value: Option<&str>, message: &'static str) -> Result<Str, Fault> {
	value
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(Str::new)
		.ok_or_else(|| static_fault("invalid_request", message))
}

fn done(op: CiOp, value: &impl Serialize, output: String, useless: bool) -> Result<Payload, Fault> {
	let result = serde_json::to_value(value)
		.map_err(|_| static_fault("invalid_result", "ci result could not be encoded"))?;
	Ok(Payload { op, result, output: Str::new(output), useless })
}

fn render_pipeline(pipeline: &Pipeline) -> String {
	let mut output = format!("{} {} {}", pipeline.provider.id, pipeline.id, pipeline.status);
	if !pipeline.name.is_empty() {
		output.push(' ');
		output.push_str(pipeline.name.as_str());
	}
	for job in pipeline.as_slice() {
		output.push('\n');
		output.push_str(job.id.as_str());
		output.push(' ');
		output.push_str(job.status.into_str());
		if !job.name.is_empty() {
			output.push(' ');
			output.push_str(job.name.as_str());
		}
	}
	output
}

fn host_fault(error: CiError) -> Fault {
	let code = match &error {
		CiError::ProviderNotFound { .. } => "provider_not_found",
		CiError::PipelineNotFound { .. } => "pipeline_not_found",
		CiError::JobNotFound { .. } => "job_not_found",
		CiError::UnexpectedJob { .. } => "unexpected_job",
		CiError::MissingJob { .. } => "missing_job",
		CiError::Unsupported { .. } => "unsupported",
		CiError::Rejected { .. } => "rejected",
		CiError::Unavailable { .. } => "unavailable",
		CiError::Unregistered { .. } => "unregistered",
		CiError::NotApproved { .. } => "not_approved",
		CiError::NotCapable { .. } => "not_capable",
		CiError::TooManyInputs { .. } => "too_many_inputs",
		CiError::InputTooLong { .. } => "input_too_long",
		CiError::InputInvalid => "input_invalid",
		CiError::Id(_) => "invalid_id",
		CiError::Route(_) => "route",
	};
	Fault { code: Str::new_static(code), message: Str::new(error.to_string()) }
}

const fn static_fault(code: &'static str, message: &'static str) -> Fault {
	Fault { code: Str::new_static(code), message: Str::new_static(message) }
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
		expected: sf!("one committed ci argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  None,
		found:    Some(message),
	}
}

#[cfg(test)]
mod tests {
	use std::{
		future::Future,
		sync::{
			Arc,
			atomic::{AtomicU32, Ordering},
		},
	};

	use omp_ci::{
		ArtifactPage, Capabilities, CiError, CiHost, CiOp, CiProvider, CiStatus, Job, LogPage,
		Pipeline, RunRef, Trigger,
	};
	use omp_core::Str;
	use omp_tool::Tool as _;

	use super::{Params, dispatch, tool};

	#[derive(Clone)]
	struct Script {
		statuses:     Arc<Vec<CiStatus>>,
		status_calls: Arc<AtomicU32>,
		logs:         Arc<AtomicU32>,
	}

	impl CiHost for Script {
		fn capabilities(
			&self,
			_: &CiProvider,
		) -> impl Future<Output = Result<Capabilities, CiError>> + Send {
			async { Ok(Capabilities { log_stream: true, ..Capabilities::none() }) }
		}

		fn status(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			let index = self.status_calls.fetch_add(1, Ordering::SeqCst) as usize;
			let status = self
				.statuses
				.get(index)
				.copied()
				.unwrap_or(CiStatus::Failure);
			let provider = run.provider.clone();
			let id = run.pipeline.clone();
			async move { Ok(Pipeline::new(provider, id, "ci", status, [Job::new("3", "test", status)])) }
		}

		fn artifacts(
			&self,
			_: &RunRef,
		) -> impl Future<Output = Result<ArtifactPage, CiError>> + Send {
			async {
				Err(CiError::NotCapable {
					op:       CiOp::Artifacts,
					provider: Str::new_static("github.com"),
				})
			}
		}

		fn log_stream(&self, run: &RunRef) -> impl Future<Output = Result<LogPage, CiError>> + Send {
			self.logs.fetch_add(1, Ordering::SeqCst);
			let run = run.clone();
			async move { Ok(LogPage::new(run, "failed job", true)) }
		}

		fn cancel(&self, _: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			async {
				Err(CiError::NotCapable {
					op:       CiOp::Cancel,
					provider: Str::new_static("github.com"),
				})
			}
		}

		fn rerun_failed(&self, _: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			async { Err(CiError::Unsupported { op: CiOp::RerunFailed }) }
		}

		fn rerun_job(&self, _: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
			async { Err(CiError::Unsupported { op: CiOp::RerunJob }) }
		}

		fn trigger(&self, _: Trigger) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			async { Err(CiError::Unsupported { op: CiOp::Trigger }) }
		}

		fn play_manual(&self, _: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
			async { Err(CiError::Unsupported { op: CiOp::PlayManual }) }
		}
	}

	fn params(watch: bool) -> Params {
		Params {
			op: CiOp::Status,
			provider: Some(Str::new_static("github.com")),
			pipeline: Some(Str::new_static("42")),
			job: None,
			watch,
			inputs: Vec::new(),
		}
	}

	#[test]
	fn revision_one_schema_names_the_contract_fields_and_watch_defaults_false() {
		let built = tool(Script {
			statuses:     Arc::new(vec![CiStatus::Success]),
			status_calls: Arc::new(AtomicU32::new(0)),
			logs:         Arc::new(AtomicU32::new(0)),
		});
		assert_eq!(built.spec().name.as_str(), "ci");
		assert_eq!(built.spec().rev.n, 1);
		assert!(
			built
				.spec()
				.effects
				.exec
				.as_ref()
				.is_some_and(|exec| { exec.network && exec.commands.is_empty() })
		);
		let schema: serde_json::Value =
			serde_json::from_slice(&built.spec().schema).expect("ci schema is JSON");
		let mut names = schema["properties"]
			.as_object()
			.expect("object")
			.keys()
			.filter(|name| !matches!(name.as_str(), "i" | "notrunc"))
			.map(String::as_str)
			.collect::<Vec<_>>();
		names.sort_unstable();
		assert_eq!(names, ["inputs", "job", "op", "pipeline", "provider", "watch"]);
		let parsed: Params = serde_json::from_value(serde_json::json!({
			"op": "status",
			"provider": "github.com",
			"pipeline": "42",
		}))
		.expect("params");
		assert!(!parsed.watch);
		assert!(parsed.inputs.is_empty());
	}

	#[tokio::test]
	async fn watch_false_reads_status_once_and_does_not_read_logs() {
		let host = Script {
			statuses:     Arc::new(vec![CiStatus::Failure]),
			status_calls: Arc::new(AtomicU32::new(0)),
			logs:         Arc::new(AtomicU32::new(0)),
		};
		let (tx, _rx) = flume::bounded(4);
		let payload = dispatch(&host, params(false), std::time::Duration::from_millis(1), &tx)
			.await
			.expect("status");
		assert_eq!(host.status_calls.load(Ordering::SeqCst), 1);
		assert_eq!(host.logs.load(Ordering::SeqCst), 0);
		assert!(payload.output.as_str().contains("failure"), "{}", payload.output);
	}

	#[tokio::test]
	async fn watch_true_polls_until_terminal_then_reads_failed_job_logs() {
		let host = Script {
			statuses:     Arc::new(vec![CiStatus::Running, CiStatus::Failure]),
			status_calls: Arc::new(AtomicU32::new(0)),
			logs:         Arc::new(AtomicU32::new(0)),
		};
		let (tx, _rx) = flume::bounded(4);
		let payload = dispatch(&host, params(true), std::time::Duration::from_millis(1), &tx)
			.await
			.expect("watch");
		assert_eq!(host.status_calls.load(Ordering::SeqCst), 2);
		assert_eq!(host.logs.load(Ordering::SeqCst), 1);
		assert!(payload.output.as_str().contains("failed job"), "{}", payload.output);
	}
}
