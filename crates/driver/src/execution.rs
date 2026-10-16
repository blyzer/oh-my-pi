//! Runtime composition for the concrete Local and remote execution backends.
//!
//! Placement remains pure in `omp-exec::place`; this module owns the runtime
//! handoff and the pre-acceptance-only fallback rule.

use std::{future::Future, io};

use omp_exec::{
	BackendCapabilities, BackendId, ExecutionBackend, ExecutionError, ExecutionErrorKind,
	ExecutionEvent, ExecutionPolicy, ExecutionRequest, ExecutionResult, ExecutionRun, FallbackCause,
	FallbackPolicy, Placement, RemoteCancel, Stage, fallback_eligible, place,
};
use tokio_util::sync::CancellationToken;

/// An enum over two concrete backend handles, avoiding a dynamic dispatch
/// boundary on the execution hot path.
#[derive(Clone)]
pub enum BackendPair<L, R> {
	/// The local backend.
	Local(L),
	/// The remote backend.
	Remote(R),
}

/// A run from either concrete backend.
pub enum BackendRun<L, R> {
	/// A local run.
	Local(L),
	/// A remote run.
	Remote(R),
}

impl<L, R> ExecutionBackend for BackendPair<L, R>
where
	L: ExecutionBackend,
	R: ExecutionBackend,
{
	type Run = BackendRun<L::Run, R::Run>;

	fn id(&self) -> &BackendId<str> {
		match self {
			Self::Local(backend) => backend.id(),
			Self::Remote(backend) => backend.id(),
		}
	}

	fn capabilities(&self) -> &BackendCapabilities {
		match self {
			Self::Local(backend) => backend.capabilities(),
			Self::Remote(backend) => backend.capabilities(),
		}
	}

	fn start(
		&self,
		request: ExecutionRequest,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		let backend = self.clone();
		async move {
			match backend {
				Self::Local(backend) => backend.start(request).await.map(BackendRun::Local),
				Self::Remote(backend) => backend.start(request).await.map(BackendRun::Remote),
			}
		}
	}

	fn reattach(
		&self,
		handle: omp_exec::Reattach,
	) -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_ {
		let backend = self.clone();
		async move {
			match backend {
				Self::Local(backend) => backend.reattach(handle).await.map(BackendRun::Local),
				Self::Remote(backend) => backend.reattach(handle).await.map(BackendRun::Remote),
			}
		}
	}
}

impl<L, R> ExecutionRun for BackendRun<L, R>
where
	L: ExecutionRun + Sync,
	R: ExecutionRun + Sync,
{
	fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_ {
		async move {
			match self {
				Self::Local(run) => run.next_event().await,
				Self::Remote(run) => run.next_event().await,
			}
		}
	}

	fn cancel(&self) -> impl Future<Output = omp_exec::CancelOutcome> + Send + '_ {
		async move {
			match self {
				Self::Local(run) => run.cancel().await,
				Self::Remote(run) => run.cancel().await,
			}
		}
	}

	fn detach(self) -> omp_exec::Reattach {
		match self {
			Self::Local(run) => run.detach(),
			Self::Remote(run) => run.detach(),
		}
	}
}

/// Executes one request using pure placement plus the pre-acceptance fallback
/// rule. A command that has emitted `Accepted` is never rerun locally.
pub async fn run_with_fallback<L, R>(
	request: ExecutionRequest,
	local: &L,
	remote: &R,
	policy: &ExecutionPolicy,
	cancel: &CancellationToken,
) -> Result<ExecutionResult, ExecutionError>
where
	L: ExecutionBackend,
	R: ExecutionBackend,
{
	let placement = place(&request, &[remote.capabilities()], policy)
		.map_err(|need| ExecutionError::Unplaceable { need })?;
	match placement {
		Placement::Local => {
			let (result, _) = settle(local.start(request).await?, cancel).await;
			result
		},
		Placement::Remote { .. } => {
			let remote_result = match remote.start(request.clone()).await {
				Ok(run) => settle(run, cancel).await,
				Err(error) => (Err(error), false),
			};
			match remote_result {
				(Ok(result), _) => Ok(result),
				(Err(error), false)
					if policy.fallback != FallbackPolicy::RemoteOnly
						&& request.eligibility != omp_exec::Eligibility::RemoteRequired
						&& fallback_eligible(&error, Stage::BeforeAcceptance) =>
				{
					let kind = ExecutionErrorKind::from(&error);
					let (result, _) = settle(local.start(request).await?, cancel).await;
					let mut result = result?;
					result.fallback = Some(FallbackCause { kind });
					Ok(result)
				},
				(Err(error), _) => Err(error),
			}
		},
	}
}

async fn settle<R: ExecutionRun>(
	mut run: R,
	cancel: &CancellationToken,
) -> (Result<ExecutionResult, ExecutionError>, bool) {
	let mut accepted = false;
	loop {
		let event = tokio::select! {
			event = run.next_event() => event,
			() = cancel.cancelled() => {
				let _ = run.cancel().await;
				return (Err(ExecutionError::Cancelled { remote: RemoteCancel::Requested }), accepted);
			},
		};
		match event {
			Some(ExecutionEvent::Accepted { .. }) => accepted = true,
			Some(ExecutionEvent::Settled(result)) => return (result, accepted),
			Some(
				ExecutionEvent::Queued
				| ExecutionEvent::Executing
				| ExecutionEvent::Uploading { .. }
				| ExecutionEvent::Output(_),
			) => {},
			None => {
				return (
					Err(ExecutionError::Infrastructure {
						source: std::sync::Arc::new(io::Error::other(
							"execution ended without a settled result",
						)),
					}),
					accepted,
				);
			},
		}
	}
}
