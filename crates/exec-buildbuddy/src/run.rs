//! One BuildBuddy invocation and its paginated logs.

use std::{
	collections::VecDeque,
	future::Future,
	sync::Arc,
	time::{Duration, Instant},
};

use bytes::Bytes;
use omp_core::{CowBytes, Str};
use omp_exec::{
	CancelOutcome, ExecutionError, ExecutionEvent, ExecutionId, ExecutionResult, ExecutionRun,
	Outcome, OutputFrame, Reattach, Stream, Timing, TransferCounts,
};
use serde::{Deserialize, Serialize};

use super::{ApiError, Config, HttpClient, MAX_LOG_BYTES, post};

/// An accepted Run API invocation. Its token never travels in the action body.
pub struct BuildBuddyRun {
	config:     Arc<Config>,
	client:     HttpClient,
	invocation: Str,
	id:         ExecutionId,
	timeout:    Duration,
	started:    Instant,
	state:      RunState,
	logs:       VecDeque<Bytes>,
	log_bytes:  usize,
	terminal:   Option<ExecutionEvent>,
}

#[derive(Clone, Copy)]
enum RunState {
	Accepted,
	Executing,
	Polling,
	Settled,
	Done,
}

impl BuildBuddyRun {
	pub(crate) fn new(
		config: Arc<Config>,
		client: HttpClient,
		invocation: Str,
		id: ExecutionId,
		timeout: Duration,
	) -> Self {
		Self {
			config,
			client,
			invocation,
			id,
			timeout,
			started: Instant::now(),
			state: RunState::Accepted,
			logs: VecDeque::new(),
			log_bytes: 0,
			terminal: None,
		}
	}

	async fn poll(&self) -> Result<Option<bool>, ApiError> {
		let reply: InvocationResponse =
			post(&self.client, &self.config, "GetInvocation", &InvocationRequest {
				selector: Selector { invocation_id: &self.invocation },
			})
			.await?;
		let Some(invocation) = reply.invocation.into_iter().next() else {
			return Ok(None);
		};
		match invocation.invocation_status.as_str() {
			"COMPLETE_INVOCATION_STATUS" => Ok(Some(invocation.success)),
			"DISCONNECTED_INVOCATION_STATUS" => Err(ApiError::IncompleteInvocation),
			"PARTIAL_INVOCATION_STATUS" | "UNKNOWN_INVOCATION_STATUS" | "" => Ok(None),
			_ => Err(ApiError::IncompleteInvocation),
		}
	}

	async fn fetch_logs(&mut self) -> Result<(), ApiError> {
		let mut page = String::new();
		loop {
			let reply: LogResponse = post(&self.client, &self.config, "GetLog", &LogRequest {
				selector:   Selector { invocation_id: &self.invocation },
				page_token: &page,
			})
			.await?;
			if let Some(log) = reply.log {
				let bytes = Bytes::from(log.contents.into_bytes());
				self.log_bytes = self.log_bytes.saturating_add(bytes.len());
				if self.log_bytes > MAX_LOG_BYTES {
					return Err(ApiError::LogTooLarge);
				}
				if !bytes.is_empty() {
					self.logs.push_back(bytes);
				}
			}
			if reply.next_page_token.is_empty() {
				return Ok(());
			}
			if reply.next_page_token == page {
				return Err(ApiError::IncompleteInvocation);
			}
			page = reply.next_page_token;
		}
	}

	fn verdict(&self, code: i32) -> ExecutionEvent {
		ExecutionEvent::Settled(Ok(ExecutionResult {
			id:        self.id.clone(),
			backend:   self.config.id.clone(),
			outcome:   Outcome::Exited { code },
			logs:      None,
			artifacts: Arc::from([]),
			timing:    Timing {
				total: self.started.elapsed().as_millis().min(u32::MAX as u128) as u32,
				..Timing::default()
			},
			transfer:  TransferCounts::default(),
			cache_hit: false,
			fallback:  None,
		}))
	}
}

impl ExecutionRun for BuildBuddyRun {
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
		async move {
			loop {
				match self.state {
					RunState::Accepted => {
						self.state = RunState::Executing;
						return Some(ExecutionEvent::Accepted { operation: self.invocation.clone() });
					},
					RunState::Executing => {
						self.state = RunState::Polling;
						return Some(ExecutionEvent::Executing);
					},
					RunState::Polling => {
						if self.started.elapsed() >= self.timeout {
							self.state = RunState::Done;
							return Some(ExecutionEvent::Settled(Err(ExecutionError::TimedOut {
								limit: self.timeout,
							})));
						}
						match self.poll().await {
							Ok(Some(success)) => {
								// The API only exposes success, not an exact failure exit status.
								// Code 1 is a normalized failed-command verdict, never an infrastructure
								// failure.
								let result = self.verdict(i32::from(!success));
								if let Err(source) = self.fetch_logs().await {
									self.state = RunState::Done;
									return Some(ExecutionEvent::Settled(Err(
										ExecutionError::OutputRetrieval { source: Arc::new(source) },
									)));
								}
								self.state = RunState::Settled;
								// Retain the terminal event while draining the bounded log pages.
								self.terminal = Some(result);
							},
							Ok(None) => tokio::time::sleep(Duration::from_millis(500)).await,
							Err(source) => {
								self.state = RunState::Done;
								return Some(ExecutionEvent::Settled(Err(
									ExecutionError::Infrastructure { source: Arc::new(source) },
								)));
							},
						}
					},
					RunState::Settled => {
						if let Some(bytes) = self.logs.pop_front() {
							return Some(ExecutionEvent::Output(OutputFrame {
								stream: Stream::Stdout,
								bytes:  CowBytes::owned(bytes),
							}));
						}
						self.state = RunState::Done;
						return self.terminal.take();
					},
					RunState::Done => return None,
				}
			}
		}
	}

	fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_ {
		async move {
			#[cfg(feature = "buildbuddy-cancel")]
			if let Some(canceller) = &self.config.canceller {
				return match canceller.cancel_invocation(&self.invocation).await {
					Ok(()) => CancelOutcome::Requested,
					Err(status) if status.code() == tonic::Code::Unimplemented => {
						CancelOutcome::Unsupported
					},
					Err(_) => CancelOutcome::Failed,
				};
			}
			CancelOutcome::Unsupported
		}
	}

	fn detach(self) -> Reattach {
		Reattach { backend: self.config.id.clone(), operation: self.invocation, id: self.id }
	}
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Selector<'a> {
	invocation_id: &'a str,
}

#[derive(Serialize)]
struct InvocationRequest<'a> {
	selector: Selector<'a>,
}

#[derive(Deserialize)]
struct InvocationResponse {
	invocation: Vec<Invocation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invocation {
	success:           bool,
	#[serde(default)]
	invocation_status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LogRequest<'a> {
	selector:   Selector<'a>,
	page_token: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogResponse {
	log:             Option<Log>,
	#[serde(default)]
	next_page_token: String,
}

#[derive(Deserialize)]
struct Log {
	contents: String,
}
