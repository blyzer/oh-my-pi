//! A deterministic, in-memory [`DelegatedExecutor`] for contract tests.
//!
//! It spawns nothing, reads no file and touches no credential: behavior is a
//! script of [`Step`]s per turn plus a [`MockConfig`] of deliberate
//! misbehavior, and everything the host did to it is recorded in a
//! [`MockRecord`] that tests inspect through a [`MockHandle`].
//!
//! Ids are deterministic. The Nth session opened is `mock-session-N`; the Nth
//! turn started on a session is `turn-N`, so scripts can name them.
//!
//! Like a real CLI, the mock blocks on an unanswered permission prompt: after
//! it emits [`DelegatedEvent::Permission`], [`DelegatedSession::next_event`]
//! does not return until [`DelegatedSession::answer`] was called.

use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};

use omp_core::sf;
use parking_lot::Mutex;

use super::{
	AuthStatus, Capabilities, CliSessionId, DelegatedEvent, DelegatedExecutor, DelegatedPolicy,
	DelegatedPrompt, DelegatedSession, ExecutionMode, ExecutorError, ExitFact, FrameFault,
	LifecycleEvent, PermissionAnswer, ProbeReport, PromptId, PromptSection, SessionInfo,
	SessionRequest, TeardownFault, TurnAccepted, TurnEnd, TurnId, TurnInput,
};

/// One scripted step of a turn.
#[derive(Clone, Debug)]
pub enum Step {
	/// Deliver this event.
	Event(DelegatedEvent),
	/// Stay silent for this long, then continue with the next step.
	Sleep(Duration),
	/// Deliver an undecodable frame.
	Malformed(FrameFault),
	/// The process dies.
	Crash(ExitFact),
	/// The process never says anything again.
	Hang,
}

/// What the mock's probe reports.
#[derive(Clone, Copy, Debug)]
pub enum ProbeScript {
	/// The executable is not installed.
	Missing,
	/// The executable is installed and reports this.
	Report(ProbeReport),
}

/// How the mock answers a resume request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumeScript {
	/// Resume a session this mock opened earlier; otherwise open a fresh one
	/// and say so.
	Honor,
	/// Ignore the request and open a fresh session.
	Fresh,
	/// Claim a resume under a new identity, requested or not.
	ClaimResumed,
}

/// How the mock reacts to a cooperative interrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptScript {
	/// End the turn as [`TurnEnd::Interrupted`].
	Cooperative,
	/// Do nothing, as a wedged CLI would.
	Ignore,
}

/// Deliberate misbehavior of the mock.
#[derive(Clone, Debug)]
pub struct MockConfig {
	/// What the probe reports.
	pub probe:            ProbeScript,
	/// Report this policy as applied instead of the requested one.
	pub applied_policy:   Option<DelegatedPolicy>,
	/// Report this working directory instead of the requested one.
	pub reported_workdir: Option<PathBuf>,
	/// How resume requests are answered.
	pub resume:           ResumeScript,
	/// How interrupts are answered.
	pub interrupt:        InterruptScript,
	/// Append bytes to the first prompt section before "forwarding" it, as a
	/// rewriting adapter would.
	pub rewrite_prompt:   bool,
	/// Make teardown fail.
	pub teardown_fails:   bool,
}

impl Default for MockConfig {
	fn default() -> Self {
		Self {
			probe:            ProbeScript::Report(ProbeReport {
				version:  super::CliVersion::new(1, 0, 0),
				auth:     AuthStatus::Authenticated,
			}),
			applied_policy:   None,
			reported_workdir: None,
			resume:           ResumeScript::Honor,
			interrupt:        InterruptScript::Cooperative,
			rewrite_prompt:   false,
			teardown_fails:   false,
		}
	}
}

/// One `open` call the mock received.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRecord {
	/// Requested mode.
	pub mode:    ExecutionMode,
	/// Requested working directory.
	pub workdir: PathBuf,
	/// Requested policy.
	pub policy:  DelegatedPolicy,
	/// Requested resume.
	pub resume:  Option<CliSessionId>,
}

/// Everything the host did to the mock.
#[derive(Clone, Debug, Default)]
pub struct MockRecord {
	/// Probes performed.
	pub probes:       u32,
	/// `open` calls, in order.
	pub opens:        Vec<OpenRecord>,
	/// The sections delivered by each started turn, exactly as received.
	pub prompts:      Vec<Vec<PromptSection>>,
	/// Permission answers received.
	pub answers:      Vec<(PromptId, PermissionAnswer)>,
	/// Cooperative interrupts received.
	pub interrupts:   u32,
	/// Explicit `terminate` calls received.
	pub terminations: u32,
	/// Sessions dropped without a prior `terminate` (the kill-on-drop path).
	pub dropped_live: u32,
	/// Sessions this mock has opened and may resume.
	pub sessions:     Vec<CliSessionId>,
}

/// Shared, cloneable view of a mock's [`MockRecord`].
#[derive(Clone, Debug, Default)]
pub struct MockHandle(Arc<Mutex<MockRecord>>);

impl MockHandle {
	/// A copy of the record as it is now.
	#[must_use]
	pub fn snapshot(&self) -> MockRecord {
		self.0.lock().clone()
	}
}

/// The mock backend.
pub struct MockExecutor {
	caps:   Capabilities,
	config: MockConfig,
	script: Arc<Mutex<VecDeque<Vec<Step>>>>,
	record: MockHandle,
}

impl MockExecutor {
	/// Creates a mock that declares `caps` and behaves well.
	#[must_use]
	pub fn new(caps: Capabilities) -> Self {
		Self {
			caps,
			config: MockConfig::default(),
			script: Arc::new(Mutex::new(VecDeque::new())),
			record: MockHandle::default(),
		}
	}

	/// Replaces the misbehavior configuration.
	#[must_use]
	pub fn with_config(mut self, config: MockConfig) -> Self {
		self.config = config;
		self
	}

	/// Queues the steps of the next turn that will be started on any session.
	pub fn push_turn(&self, steps: impl Into<Vec<Step>>) {
		self.script.lock().push_back(steps.into());
	}

	/// A handle on what the host does to this mock.
	#[must_use]
	pub fn handle(&self) -> MockHandle {
		self.record.clone()
	}
}

impl DelegatedExecutor for MockExecutor {
	type Session = MockSession;

	fn capabilities(&self) -> &Capabilities {
		&self.caps
	}

	async fn probe(&self) -> Result<ProbeReport, ExecutorError> {
		self.record.0.lock().probes += 1;
		match self.config.probe {
			ProbeScript::Missing => {
				Err(ExecutorError::ExecutableMissing { executor: self.caps.executor.clone() })
			},
			ProbeScript::Report(report) => Ok(report),
		}
	}

	async fn open<'a>(
		&'a self,
		request: &'a SessionRequest,
	) -> Result<(Self::Session, SessionInfo), ExecutorError> {
		let mut record = self.record.0.lock();
		record.opens.push(OpenRecord {
			mode:    request.mode,
			workdir: request.workdir.clone(),
			policy:  request.policy,
			resume:  request.resume.clone(),
		});
		let fresh = || CliSessionId::new(sf!("mock-session-{}", record.sessions.len() + 1));
		let (session, resumed) = match (&request.resume, self.config.resume) {
			(Some(wanted), ResumeScript::Honor) if record.sessions.contains(wanted) => {
				(wanted.clone(), true)
			},
			(_, ResumeScript::ClaimResumed) => (fresh(), true),
			_ => (fresh(), false),
		};
		if !record.sessions.contains(&session) {
			record.sessions.push(session.clone());
		}
		drop(record);
		let info = SessionInfo {
			session,
			resumed,
			workdir: self
				.config
				.reported_workdir
				.clone()
				.unwrap_or_else(|| request.workdir.clone()),
			policy: self.config.applied_policy.unwrap_or(request.policy),
		};
		let live = MockSession {
			executor:       self.caps.executor.clone(),
			config:         self.config.clone(),
			script:         Arc::clone(&self.script),
			record:         self.record.clone(),
			steps:          VecDeque::new(),
			turn:           None,
			turns_started:  0,
			frame:          0,
			pending_prompt: None,
			interrupted:    false,
			terminated:     false,
		};
		Ok((live, info))
	}
}

/// A live mock session.
pub struct MockSession {
	executor:       super::ExecutorId,
	config:         MockConfig,
	script:         Arc<Mutex<VecDeque<Vec<Step>>>>,
	record:         MockHandle,
	steps:          VecDeque<Step>,
	turn:           Option<TurnId>,
	turns_started:  u32,
	frame:          u64,
	pending_prompt: Option<PromptId>,
	interrupted:    bool,
	terminated:     bool,
}

impl MockSession {
	fn crashed(&self, exit: ExitFact) -> ExecutorError {
		ExecutorError::Crashed { executor: self.executor.clone(), exit }
	}
}

impl DelegatedSession for MockSession {
	async fn start_turn<'a>(&'a mut self, input: &'a TurnInput) -> Result<TurnAccepted, ExecutorError> {
		self.turns_started += 1;
		let turn = TurnId::new(sf!("turn-{}", self.turns_started));
		let mut delivered = input.prompt.sections().to_vec();
		if self.config.rewrite_prompt
			&& let Some(first) = delivered.first_mut()
		{
			first.text = sf!("{} (paraphrased)", first.text);
		}
		let forwarded = DelegatedPrompt::digest_of(&delivered, input.prompt.bands());
		self.record.0.lock().prompts.push(delivered);
		self.steps = self.script.lock().pop_front().unwrap_or_default().into();
		self.turn = Some(turn.clone());
		self.interrupted = false;
		self.pending_prompt = None;
		Ok(TurnAccepted { turn, forwarded })
	}

	async fn next_event(&mut self) -> Result<DelegatedEvent, ExecutorError> {
		if self.interrupted {
			match self.config.interrupt {
				InterruptScript::Cooperative => {
					self.interrupted = false;
					self.steps.clear();
					self.pending_prompt = None;
					if let Some(turn) = self.turn.clone() {
						return Ok(DelegatedEvent::Lifecycle(LifecycleEvent::TurnCompleted {
							turn,
							outcome: TurnEnd::Interrupted,
						}));
					}
				},
				InterruptScript::Ignore => std::future::pending::<()>().await,
			}
		}
		if self.pending_prompt.is_some() {
			std::future::pending::<()>().await;
		}
		loop {
			// Steps are cloned from the front and consumed only once delivered, so
			// dropping this future mid-sleep loses nothing (cancel-safe).
			match self.steps.front().cloned() {
				None => return Err(self.crashed(ExitFact::Unknown)),
				Some(Step::Hang) => std::future::pending::<()>().await,
				Some(Step::Sleep(span)) => {
					tokio::time::sleep(span).await;
					self.steps.pop_front();
				},
				Some(Step::Crash(exit)) => return Err(self.crashed(exit)),
				Some(Step::Malformed(fault)) => {
					return Err(ExecutorError::MalformedStream {
						executor: self.executor.clone(),
						frame:    self.frame,
						fault,
					});
				},
				Some(Step::Event(event)) => {
					self.steps.pop_front();
					self.frame += 1;
					if let DelegatedEvent::Permission(request) = &event {
						self.pending_prompt = Some(request.prompt.clone());
					}
					return Ok(event);
				},
			}
		}
	}

	async fn answer<'a>(
		&'a mut self,
		prompt: &'a PromptId,
		answer: PermissionAnswer,
	) -> Result<(), ExecutorError> {
		self.record.0.lock().answers.push((prompt.clone(), answer));
		if self.pending_prompt.as_ref() == Some(prompt) {
			self.pending_prompt = None;
		}
		Ok(())
	}

	async fn interrupt(&mut self) -> Result<(), ExecutorError> {
		self.record.0.lock().interrupts += 1;
		self.interrupted = true;
		Ok(())
	}

	async fn terminate(&mut self) -> Result<(), ExecutorError> {
		if self.terminated {
			return Ok(());
		}
		self.terminated = true;
		self.record.0.lock().terminations += 1;
		if self.config.teardown_fails {
			return Err(ExecutorError::TeardownFailed {
				executor: self.executor.clone(),
				fault:    TeardownFault::Unreaped,
			});
		}
		Ok(())
	}
}

impl Drop for MockSession {
	fn drop(&mut self) {
		if !self.terminated {
			self.record.0.lock().dropped_live += 1;
		}
	}
}
