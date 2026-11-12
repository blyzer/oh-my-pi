//! Host for the AI developer workflow domain.
//!
//! `omp-adw` is a pure state machine: it decides which phase may dispatch, how
//! many attempts one has consumed, and what a rejection invalidates. It
//! performs no I/O. This module is the other half — it reads the project's
//! workflow declaration, resolves the execution posture the domain compares
//! requirements against, dispatches what the domain hands back, and reports
//! outcomes in.
//!
//! The split is load-bearing: acceptance is host-owned, so gate results arrive
//! here as observed evidence and go *into* the domain, rather than the domain
//! reaching out for them.

pub mod definition;
pub mod production;
pub mod remote;

use std::{error::Error as StdError, future::Future, path::Path};

pub use definition::{DefinitionError, PhaseSpec, WorkflowDefinition};
use omp_adw::{
	OnReject, Outcome, Phase, PhaseName, Posture, Run, RunError, SelectedInput, Step,
	replay::{ReplayError, ReplayEvent, replay},
};
use omp_core::Str;
use omp_journal::runlog::{
	PhaseDispatch, PhaseSettle, RunDone, RunHalt, RunLog, RunLogError, RunRecord, SettleStatus,
};
use tokio_util::sync::CancellationToken;

/// What a dispatched phase reported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseResult {
	/// Whether the phase met its acceptance condition.
	pub accepted: bool,
	/// Why the phase was rejected, forwarded verbatim as the next attempt's
	/// correction. Empty when accepted.
	pub feedback: Str,
}

/// The disposition of one settled phase attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum PhaseStatus {
	/// The phase met its acceptance condition.
	Accepted,
	/// The phase completed but did not meet its acceptance condition.
	Rejected,
	/// The phase's host stopped before it produced a result.
	Failed,
}

/// One settled phase, recorded in dispatch order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseRecord {
	/// The phase that ran.
	pub phase:    Str,
	/// The attempt ordinal this record describes.
	pub attempt:  u32,
	/// The disposition of this attempt.
	pub status:   PhaseStatus,
	/// What the run decided in response, rendered for the operator.
	pub decision: Str,
	/// What the host reported for this attempt (a code phase's captured
	/// output, a reviewer's reason). Empty when the host stopped before
	/// producing a result.
	pub feedback: Str,
}

/// The typed failure of one phase dispatch.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("workflow phase `{phase}` attempt {attempt} failed")]
pub struct PhaseFailure<E: StdError + 'static> {
	/// The phase whose host dispatch failed.
	pub phase:   Str,
	/// The attempt ordinal whose host dispatch failed.
	pub attempt: u32,
	/// The host error that stopped this run.
	#[source]
	pub source:  E,
}

/// How a run finished.
#[derive(Debug, Eq, PartialEq)]
pub enum RunStatus<E: StdError + 'static> {
	/// Every phase was accepted.
	Accepted,
	/// The run halted without accepting every phase.
	Halted,
	/// A phase required a posture the host did not resolve.
	Blocked,
	/// A dispatched phase's host failed.
	Failed(PhaseFailure<E>),
	/// The operator cancelled the run.
	Cancelled,
}

/// The settled result of one workflow run.
#[derive(Debug, Eq, PartialEq)]
pub struct RunExit<E: StdError + 'static> {
	/// Process exit status: 0 accepted, 1 halted, blocked, or failed, 130
	/// cancelled.
	pub code:    u8,
	/// How the run finished.
	pub status:  RunStatus<E>,
	/// Every settled attempt, in dispatch order.
	pub records: Vec<PhaseRecord>,
	/// Why the run stopped, when it did not accept or fail.
	pub reason:  Option<Str>,
}

/// Production seams for posture resolution and phase dispatch.
///
/// Deliberately narrow: the domain already decided *what* runs next, so a host
/// only resolves the posture it can honestly report and executes one phase.
pub trait AdwHost: Sync {
	/// Typed host failure.
	type Error: StdError + Send + Sync + 'static;

	/// The project root this run executes against.
	fn project_root(&self) -> &Path;

	/// Reports the posture the host actually resolved.
	///
	/// Reported once, before the first dispatch. A host that cannot determine
	/// its posture must say so by failing rather than guessing: the domain
	/// treats an unreported posture as unverified and refuses phases that
	/// declare requirements, which is the safe direction.
	fn posture(&self) -> Result<Posture, Self::Error>;
	/// Reports the actual placement's posture for one phase. Hosts with one
	/// execution environment inherit the run-wide value.
	fn phase_posture(&self, _phase: &Phase, _spec: &PhaseSpec) -> Result<Posture, Self::Error> {
		self.posture()
	}

	/// Runs one phase and reports what it produced.
	fn dispatch<'a>(
		&'a self,
		phase: &'a Phase,
		spec: &'a PhaseSpec,
		attempt: u32,
		correction: Option<&'a Str>,
		inputs: &'a [SelectedInput],
		cancel: &'a CancellationToken,
	) -> impl Future<Output = Result<PhaseResult, Self::Error>> + Send + 'a;
}

/// A workflow run that could not proceed.
#[derive(Debug, thiserror::Error)]
pub enum Error<E: StdError + 'static> {
	/// The project declares no such workflow, or it is invalid.
	#[error("workflow definition could not be loaded")]
	Definition(#[from] DefinitionError),
	/// The domain refused a transition.
	#[error("workflow run rejected a transition")]
	Transition(#[from] RunError),
	/// Posture resolution failed before a phase could be recorded.
	#[error("workflow runtime posture resolution failed")]
	Posture(#[source] E),
	/// Journal write failed.
	#[error("workflow run log write failed")]
	Journal(#[from] RunLogError),
	/// Replay of recorded journal events failed.
	#[error("workflow run log replay failed")]
	Replay(#[from] ReplayError),
}

/// Drives one workflow to acceptance, halt, block, failure, or cancellation.
///
/// # Errors
///
/// Returns [`Error`] when posture resolution fails before any phase can be
/// recorded or when the domain refuses a transition. A halted, blocked, failed,
/// or cancelled run is a normal outcome carried in [`RunExit`].
pub async fn run<H: AdwHost>(
	definition: &WorkflowDefinition,
	max_attempts: u32,
	host: &H,
	cancel: &CancellationToken,
) -> Result<RunExit<H::Error>, Error<H::Error>> {
	run_with_log(definition, max_attempts, host, cancel, None).await
}

/// Drives one workflow, recording events to an optional run log.
///
/// # Errors
///
/// Returns [`Error`] when posture resolution fails, the domain refuses a
/// transition, or an append to `log` fails.
pub async fn run_with_log<H: AdwHost>(
	definition: &WorkflowDefinition,
	max_attempts: u32,
	host: &H,
	cancel: &CancellationToken,
	mut log: Option<&mut RunLog>,
) -> Result<RunExit<H::Error>, Error<H::Error>> {
	let posture = host.posture().map_err(Error::Posture)?;
	let mut run = Run::new(definition.workflow().clone())
		.with_max_attempts(max_attempts)
		.with_posture(posture);
	let mut records = Vec::new();
	// The domain reports *that* a run halted; only the phase's declared
	// rejection route says why. Captured where both are in scope so the
	// operator is told the actual cause, not a plausible one.
	let mut halt_reason = None;

	loop {
		if cancel.is_cancelled() {
			if let Some(log) = log.as_mut() {
				log.append(&RunRecord::Halt(RunHalt { reason: Str::new_static("cancelled") }))?;
			}
			return Ok(cancelled(records));
		}
		let mut posture_error = None;
		let step = run.next_step_with_posture(|phase| {
			let spec = definition.spec(&phase.name)?;
			match host.phase_posture(phase, spec) {
				Ok(posture) => Some(posture),
				Err(error) => {
					posture_error = Some(error);
					None
				},
			}
		});
		if let Some(error) = posture_error {
			return Err(Error::Posture(error));
		}
		match step {
			Step::Run { phase, attempt, correction, inputs } => {
				let Some(spec) = definition.spec(&phase.name) else {
					// Unreachable by construction: the loader rejects a phase
					// without execution detail. Treated as a halt rather than a
					// panic so a future loader change degrades visibly.
					if let Some(log) = log.as_mut() {
						log.append(&RunRecord::Halt(RunHalt {
							reason: Str::from(format!(
								"phase `{}` carries no execution detail",
								phase.name.as_str()
							)),
						}))?;
					}
					return Ok(halted(
						records,
						Some(Str::from(format!(
							"phase `{}` carries no execution detail",
							phase.name.as_str()
						))),
					));
				};
				if let Some(log) = log.as_mut() {
					let executor = match spec {
						PhaseSpec::Prompt { executor: Some(id), .. } => id.clone(),
						_ => Str::new_static("native"),
					};
					let posture_str = Str::from(format!("{:?}", posture));
					let input_versions = inputs
						.iter()
						.map(|inp| omp_journal::runlog::InputVersion {
							phase:   inp.phase.as_str().into(),
							version: inp.version,
						})
						.collect();
					log.append(&RunRecord::Dispatch(PhaseDispatch {
						phase: phase.name.as_str().into(),
						attempt,
						inputs: input_versions,
						executor,
						posture: posture_str,
						effects: Vec::new(),
					}))?;
				}
				let result = match host
					.dispatch(&phase, spec, attempt, correction.as_ref(), &inputs, cancel)
					.await
				{
					Ok(result) => result,
					Err(_) if cancel.is_cancelled() => {
						if let Some(log) = log.as_mut() {
							log.append(&RunRecord::Halt(RunHalt {
								reason: Str::new_static("cancelled"),
							}))?;
						}
						return Ok(cancelled(records));
					},
					Err(source) => {
						if let Some(log) = log.as_mut() {
							log.append(&RunRecord::Halt(RunHalt {
								reason: Str::from(format!("phase failed: {source}")),
							}))?;
						}
						records.push(PhaseRecord {
							phase: Str::new(phase.name.as_str()),
							attempt,
							status: PhaseStatus::Failed,
							decision: Str::new_static("failed"),
							feedback: Str::default(),
						});
						return Ok(failed(records, phase.name.as_str(), attempt, source));
					},
				};
				// A phase stopped by cancellation did not fail on its merits:
				// feeding it back as a rejection would schedule (and report) a
				// retry the run will never dispatch.
				if cancel.is_cancelled() {
					if let Some(log) = log.as_mut() {
						log.append(&RunRecord::Halt(RunHalt { reason: Str::new_static("cancelled") }))?;
					}
					return Ok(cancelled(records));
				}
				let feedback = result.feedback.clone();
				let outcome = if result.accepted {
					run.accept(&phase.name)?
				} else {
					run.reject(&phase.name, result.feedback.clone())?
				};
				if let Some(log) = log.as_mut() {
					log.append(&RunRecord::Settle(PhaseSettle {
						phase: phase.name.as_str().into(),
						attempt,
						status: if result.accepted {
							SettleStatus::Accepted
						} else {
							SettleStatus::Rejected
						},
						feedback: result.feedback.clone(),
					}))?;
				}
				if matches!(outcome, Outcome::Halted { .. }) {
					halt_reason = Some(match phase.on_reject {
						OnReject::Halt => Str::from(format!(
							"phase `{}` was rejected and halts the run",
							phase.name.as_str()
						)),
						OnReject::Retry => Str::from(format!(
							"phase `{}` exhausted its {max_attempts} attempts",
							phase.name.as_str()
						)),
						OnReject::Correct => Str::from(format!(
							"phase `{}` was rejected with no upstream phase to correct",
							phase.name.as_str()
						)),
					});
				}
				records.push(PhaseRecord {
					phase: Str::new(phase.name.as_str()),
					attempt,
					status: if result.accepted {
						PhaseStatus::Accepted
					} else {
						PhaseStatus::Rejected
					},
					decision: describe(&outcome),
					feedback,
				});
			},
			// Dispatch is serial, so nothing is ever in flight here; a wait
			// with an empty ready set means the graph cannot progress.
			Step::Wait => {
				if let Some(log) = log.as_mut() {
					log.append(&RunRecord::Halt(RunHalt {
						reason: Str::new_static("no phase can proceed"),
					}))?;
				}
				return Ok(halted(records, Some(Str::new_static("no phase can proceed"))));
			},
			Step::Blocked { error } => {
				if let Some(log) = log.as_mut() {
					log.append(&RunRecord::Halt(RunHalt { reason: Str::from(error.to_string()) }))?;
				}
				return Ok(RunExit {
					code: 1,
					status: RunStatus::Blocked,
					records,
					reason: Some(Str::from(error.to_string())),
				});
			},
			Step::Done { accepted: true } => {
				if let Some(log) = log.as_mut() {
					log.append(&RunRecord::Done(RunDone { status: Str::new_static("accepted") }))?;
				}
				return Ok(RunExit { code: 0, status: RunStatus::Accepted, records, reason: None });
			},
			Step::Done { accepted: false } => {
				if let Some(log) = log.as_mut() {
					log.append(&RunRecord::Halt(RunHalt {
						reason: halt_reason
							.clone()
							.unwrap_or_else(|| Str::new_static("workflow halted")),
					}))?;
				}
				return Ok(halted(records, halt_reason));
			},
		}
	}
}

/// Converts recorded [`RunRecord`]s into [`ReplayEvent`]s for the engine
/// replay.
#[must_use]
pub fn records_to_replay_events(records: &[RunRecord]) -> Vec<ReplayEvent> {
	let mut events = Vec::new();
	for record in records {
		match record {
			RunRecord::Dispatch(d) => {
				events.push(ReplayEvent::Dispatch {
					phase:   PhaseName::new(d.phase.clone()),
					attempt: d.attempt,
					posture: None,
				});
			},
			RunRecord::Settle(s) => {
				events.push(ReplayEvent::Settle {
					phase:    PhaseName::new(s.phase.clone()),
					accepted: s.status == SettleStatus::Accepted,
					feedback: s.feedback.clone(),
				});
			},
			RunRecord::Start(_)
			| RunRecord::Uncertain(_)
			| RunRecord::Halt(_)
			| RunRecord::Done(_) => {},
		}
	}
	events
}

/// Resumes a workflow execution from an existing journal log.
///
/// # Errors
///
/// Returns [`Error`] when replay fails or the resumed run cannot proceed.
pub async fn resume_from_log<H: AdwHost>(
	definition: &WorkflowDefinition,
	max_attempts: u32,
	host: &H,
	cancel: &CancellationToken,
	mut log: RunLog,
	records: &[RunRecord],
) -> Result<RunExit<H::Error>, Error<H::Error>> {
	let posture = host.posture().map_err(Error::Posture)?;
	let mut run = Run::new(definition.workflow().clone())
		.with_max_attempts(max_attempts)
		.with_posture(posture);

	let events = records_to_replay_events(records);
	let replayed = replay(&mut run, &events)?;
	if let Some((phase, _)) = replayed.in_flight {
		run.interrupt(&phase)?;
	}

	run_with_log(definition, max_attempts, host, cancel, Some(&mut log)).await
}

fn failed<E: StdError + 'static>(
	records: Vec<PhaseRecord>,
	phase: &str,
	attempt: u32,
	source: E,
) -> RunExit<E> {
	RunExit {
		code: 1,
		status: RunStatus::Failed(PhaseFailure { phase: Str::new(phase), attempt, source }),
		records,
		reason: None,
	}
}

fn cancelled<E: StdError + 'static>(records: Vec<PhaseRecord>) -> RunExit<E> {
	RunExit { code: 130, status: RunStatus::Cancelled, records, reason: None }
}

fn halted<E: StdError + 'static>(records: Vec<PhaseRecord>, reason: Option<Str>) -> RunExit<E> {
	RunExit { code: 1, status: RunStatus::Halted, records, reason }
}
fn describe(outcome: &Outcome) -> Str {
	match outcome {
		Outcome::Accepted { version, invalidated, .. } if invalidated.is_empty() => {
			Str::from(format!("accepted as v{version}"))
		},
		Outcome::Accepted { version, invalidated, .. } => Str::from(format!(
			"accepted as v{version}; invalidated {}",
			join(invalidated.iter().map(|name| name.as_str()))
		)),
		Outcome::Retried { attempt, .. } => Str::from(format!("retrying as attempt {attempt}")),
		Outcome::Rewound { phase, .. } => Str::from(format!("rewound to `{}`", phase.as_str())),
		Outcome::Halted { .. } => Str::new_static("halted the run"),
	}
}

fn join<'a>(names: impl Iterator<Item = &'a str>) -> String {
	let mut text = String::new();
	for name in names {
		if !text.is_empty() {
			text.push_str(", ");
		}
		text.push_str(name);
	}
	text
}

#[cfg(test)]
mod tests {
	use std::{convert::Infallible, fs, future::ready, path::PathBuf};

	use omp_adw::{ApprovalScope, NetworkScope, WriteScope};
	use omp_ext::workspace_trust::inventory::WORKFLOWS_DIR;

	use super::*;

	/// Cancels the run from inside its first dispatch and reports the phase
	/// rejected, as a code phase whose process tree was killed does.
	struct CancelledMidPhase {
		root: PathBuf,
	}

	impl AdwHost for CancelledMidPhase {
		type Error = Infallible;

		fn project_root(&self) -> &Path {
			&self.root
		}

		fn posture(&self) -> Result<Posture, Infallible> {
			Ok(Posture {
				write:    WriteScope::Unconfined,
				network:  NetworkScope::Unrestricted,
				approval: ApprovalScope::Write,
			})
		}

		fn dispatch<'a>(
			&'a self,
			_phase: &'a Phase,
			_spec: &'a PhaseSpec,
			_attempt: u32,
			_correction: Option<&'a Str>,
			_inputs: &'a [SelectedInput],
			cancel: &'a CancellationToken,
		) -> impl Future<Output = Result<PhaseResult, Infallible>> + Send + 'a {
			cancel.cancel();
			ready(Ok(PhaseResult { accepted: false, feedback: Str::new_static("killed") }))
		}
	}

	#[tokio::test]
	async fn a_phase_cut_short_by_cancellation_settles_the_run_without_a_retry() {
		let project = tempfile::tempdir().expect("project");
		let directory = project.path().join(WORKFLOWS_DIR);
		fs::create_dir_all(&directory).expect("workflow directory");
		fs::write(
			directory.join("slow.toml"),
			"name = \"slow\"\n\n[[phase]]\nname = \"wait\"\nkind = \"code\"\ncommand = [\"sleep\", \
			 \"60\"]\n",
		)
		.expect("workflow file");
		let definition = definition::load(project.path(), "slow").expect("definition");
		let host = CancelledMidPhase { root: project.path().to_path_buf() };

		let exit = run(&definition, 3, &host, &CancellationToken::new())
			.await
			.expect("run");

		assert_eq!(exit.status, RunStatus::Cancelled);
		assert_eq!(exit.code, 130);
		assert!(
			exit.records.is_empty(),
			"a cancelled attempt is not a rejection: {:?}",
			exit.records
		);
	}

	#[derive(Debug, thiserror::Error)]
	#[error("provider rejected the request")]
	struct ProviderFailure;

	/// Host whose first model dispatch ends with the provider's typed error.
	struct ProviderFails {
		root: PathBuf,
	}

	impl AdwHost for ProviderFails {
		type Error = ProviderFailure;

		fn project_root(&self) -> &Path {
			&self.root
		}

		fn posture(&self) -> Result<Posture, ProviderFailure> {
			Ok(Posture {
				write:    WriteScope::Unconfined,
				network:  NetworkScope::Unrestricted,
				approval: ApprovalScope::Write,
			})
		}

		fn dispatch<'a>(
			&'a self,
			_phase: &'a Phase,
			_spec: &'a PhaseSpec,
			_attempt: u32,
			_correction: Option<&'a Str>,
			_inputs: &'a [SelectedInput],
			_cancel: &'a CancellationToken,
		) -> impl Future<Output = Result<PhaseResult, ProviderFailure>> + Send + 'a {
			ready(Err(ProviderFailure))
		}
	}

	#[tokio::test]
	async fn a_provider_failure_settles_as_a_recorded_run_outcome() {
		let project = tempfile::tempdir().expect("project");
		let directory = project.path().join(WORKFLOWS_DIR);
		fs::create_dir_all(&directory).expect("workflow directory");
		fs::write(
			directory.join("plan.toml"),
			"name = \"plan\"\n\n[[phase]]\nname = \"plan\"\nkind = \"agent\"\nprompt = \"Plan it.\"\n",
		)
		.expect("workflow file");
		let definition = definition::load(project.path(), "plan").expect("definition");
		let host = ProviderFails { root: project.path().to_path_buf() };

		let exit = run(&definition, 3, &host, &CancellationToken::new())
			.await
			.expect("provider failure settles as a recorded outcome");

		assert_eq!(exit.code, 1);
		assert_eq!(exit.records, vec![PhaseRecord {
			phase:    Str::new_static("plan"),
			attempt:  1,
			status:   PhaseStatus::Failed,
			decision: Str::new_static("failed"),
			feedback: Str::default(),
		}]);
		assert!(matches!(
			exit.status,
			RunStatus::Failed(PhaseFailure { phase, attempt: 1, source: ProviderFailure })
				if phase == Str::new_static("plan")
		));
	}

	#[derive(Debug, thiserror::Error)]
	#[error("child turn was interrupted")]
	struct CancelledHostError;

	/// Host whose child reports cancellation as a typed dispatch error.
	struct CancelledWithHostError {
		root: PathBuf,
	}

	impl AdwHost for CancelledWithHostError {
		type Error = CancelledHostError;

		fn project_root(&self) -> &Path {
			&self.root
		}

		fn posture(&self) -> Result<Posture, CancelledHostError> {
			Ok(Posture {
				write:    WriteScope::Unconfined,
				network:  NetworkScope::Unrestricted,
				approval: ApprovalScope::Write,
			})
		}

		fn dispatch<'a>(
			&'a self,
			_phase: &'a Phase,
			_spec: &'a PhaseSpec,
			_attempt: u32,
			_correction: Option<&'a Str>,
			_inputs: &'a [SelectedInput],
			cancel: &'a CancellationToken,
		) -> impl Future<Output = Result<PhaseResult, CancelledHostError>> + Send + 'a {
			cancel.cancel();
			ready(Err(CancelledHostError))
		}
	}

	#[tokio::test]
	async fn a_host_error_after_cancellation_settles_as_cancelled() {
		let project = tempfile::tempdir().expect("project");
		let directory = project.path().join(WORKFLOWS_DIR);
		fs::create_dir_all(&directory).expect("workflow directory");
		fs::write(
			directory.join("plan.toml"),
			"name = \"plan\"\n\n[[phase]]\nname = \"plan\"\nkind = \"agent\"\nprompt = \"Plan it.\"\n",
		)
		.expect("workflow file");
		let definition = definition::load(project.path(), "plan").expect("definition");
		let host = CancelledWithHostError { root: project.path().to_path_buf() };

		let exit = run(&definition, 3, &host, &CancellationToken::new())
			.await
			.expect("cancelled run");

		assert!(matches!(exit.status, RunStatus::Cancelled));
		assert!(exit.records.is_empty());
		assert!(exit.records.is_empty());
	}

	#[tokio::test]
	async fn run_records_dispatch_and_settle_in_run_log() {
		let project = tempfile::tempdir().expect("project");
		let directory = project.path().join(WORKFLOWS_DIR);
		fs::create_dir_all(&directory).expect("workflow directory");
		fs::write(
			directory.join("flow.toml"),
			"name = \"flow\"\n\n[[phase]]\nname = \"test\"\nkind = \"agent\"\nprompt = \"test it\"\n",
		)
		.expect("workflow file");
		let definition = definition::load(project.path(), "flow").expect("definition");
		struct MockHost {
			root: PathBuf,
		}
		impl AdwHost for MockHost {
			type Error = Infallible;

			fn project_root(&self) -> &Path {
				&self.root
			}

			fn posture(&self) -> Result<Posture, Infallible> {
				Ok(Posture {
					write:    WriteScope::Unconfined,
					network:  NetworkScope::Unrestricted,
					approval: ApprovalScope::Write,
				})
			}

			fn dispatch<'a>(
				&'a self,
				_phase: &'a Phase,
				_spec: &'a PhaseSpec,
				_attempt: u32,
				_correction: Option<&'a Str>,
				_inputs: &'a [SelectedInput],
				_cancel: &'a CancellationToken,
			) -> impl Future<Output = Result<PhaseResult, Infallible>> + Send + 'a {
				ready(Ok(PhaseResult { accepted: true, feedback: Str::default() }))
			}
		}
		let host = MockHost { root: project.path().to_path_buf() };
		let log_path = project.path().join("test.oms");
		let start = omp_journal::runlog::RunStart {
			run:                 Str::new_static("01JADWTEST0000000000000000"),
			workflow:            Str::new_static("flow"),
			workflow_digest:     Str::from(definition.digest()),
			input_digest:        Str::new_static(
				"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
			),
			max_attempts:        2,
			model:               Str::new_static("test"),
			model_explicit:      false,
			exec_policy:         Str::new_static("strict"),
			exec_fallback_local: false,
			workspace_trusted:   true,
			workspace_root:      Str::from(project.path().to_string_lossy().as_ref()),
			workspace_head:      None,
			created:             Str::new_static("2026-11-12T10:00:00Z"),
		};
		let mut log = RunLog::create(&log_path, start).expect("create log");
		let exit = run_with_log(&definition, 2, &host, &CancellationToken::new(), Some(&mut log))
			.await
			.expect("run");
		assert!(matches!(exit.status, RunStatus::Accepted));
		let records = RunLog::read(&log_path).expect("read log");
		assert!(records.len() >= 3); // Start, Dispatch, Settle (and Done)
		let events = records_to_replay_events(&records);
		assert_eq!(events.len(), 2); // 1 Dispatch, 1 Settle
	}
}
