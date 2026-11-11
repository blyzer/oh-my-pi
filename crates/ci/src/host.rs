//! [`CiHost`] contract.
//!
//! The trait is the read and write surface for pipelines and jobs. Futures
//! are unboxed. The trait is not object-safe; a composing crate holds a
//! concrete host, the same shape as `ForgeHost` and `ExecHost`.
//!
//! Reads are [`crate::CiHost::capabilities`], [`crate::CiHost::status`],
//! [`crate::CiHost::artifacts`], and [`crate::CiHost::log_stream`]. Writes
//! are [`crate::CiHost::cancel`], [`crate::CiHost::rerun_failed`],
//! [`crate::CiHost::rerun_job`], [`crate::CiHost::trigger`], and
//! [`crate::CiHost::play_manual`]. Each write names the [`crate::CiOp`] it
//! performs. [`crate::CiOp::mutates`] is the approval flag.

use std::future::Future;

use crate::{
	ArtifactPage, Capabilities, CiError, CiProvider, Job, LogPage, Pipeline, RunRef, Trigger,
};

/// A CI backend a caller can read and write through the contract.
///
/// Implementors perform I/O. The trait definition does not.
/// [`Self::capabilities`] is not a [`CiOp`]. It reports which operations the
/// provider implements. Status observation has no flag.
pub trait CiHost: Clone + Send + Sync + 'static {
	/// Reports the operations `provider` implements.
	///
	/// This does not mutate. It is not a [`CiOp`].
	fn capabilities(
		&self,
		provider: &CiProvider,
	) -> impl Future<Output = Result<Capabilities, CiError>> + Send;

	/// Reads a pipeline, including its jobs.
	///
	/// [`CiOp::Status`] does not mutate. A ref may name a pipeline or a job.
	/// The host returns the pipeline either way.
	fn status(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send;

	/// Reads artifact metadata.
	///
	/// [`CiOp::Artifacts`] does not mutate. A ref may name a pipeline or a job.
	fn artifacts(&self, run: &RunRef) -> impl Future<Output = Result<ArtifactPage, CiError>> + Send;

	/// Reads one log page.
	///
	/// [`CiOp::LogStream`] does not mutate. A ref may name a pipeline or a job.
	fn log_stream(&self, run: &RunRef) -> impl Future<Output = Result<LogPage, CiError>> + Send;

	/// Stops a pipeline or a job.
	///
	/// [`CiOp::Cancel`] mutates. A ref may name a pipeline or a job.
	fn cancel(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send;

	/// Reruns the failed jobs of a pipeline.
	///
	/// [`CiOp::RerunFailed`] mutates. The ref must be pipeline-scoped
	/// ([`RunRef::pipeline_scope`]).
	fn rerun_failed(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send;

	/// Reruns one job.
	///
	/// [`CiOp::RerunJob`] mutates. The ref must name a job
	/// ([`RunRef::job_scope`]).
	fn rerun_job(&self, run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send;

	/// Starts a pipeline.
	///
	/// [`CiOp::Trigger`] mutates. Inputs may be empty.
	fn trigger(&self, request: Trigger) -> impl Future<Output = Result<Pipeline, CiError>> + Send;

	/// Plays a job that is waiting for a person.
	///
	/// [`CiOp::PlayManual`] mutates. The ref must name a job
	/// ([`RunRef::job_scope`]).
	fn play_manual(&self, run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send;
}

#[cfg(test)]
mod tests {
	use std::future::Future;

	use crate::{
		ArtifactPage, Capabilities, CiError, CiHost, CiOp, CiProvider, Job, LogPage, Pipeline,
		RunRef, Trigger,
	};

	fn unsupported<T: Send>(op: CiOp) -> std::future::Ready<Result<T, CiError>> {
		std::future::ready(Err(CiError::Unsupported { op }))
	}

	#[derive(Clone)]
	struct Nop;

	impl CiHost for Nop {
		fn capabilities(
			&self,
			_provider: &CiProvider,
		) -> impl Future<Output = Result<Capabilities, CiError>> + Send {
			std::future::ready(Ok(Capabilities::none()))
		}

		fn status(&self, _run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			unsupported(CiOp::Status)
		}

		fn artifacts(
			&self,
			_run: &RunRef,
		) -> impl Future<Output = Result<ArtifactPage, CiError>> + Send {
			unsupported(CiOp::Artifacts)
		}

		fn log_stream(&self, _run: &RunRef) -> impl Future<Output = Result<LogPage, CiError>> + Send {
			unsupported(CiOp::LogStream)
		}

		fn cancel(&self, _run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			unsupported(CiOp::Cancel)
		}

		fn rerun_failed(
			&self,
			_run: &RunRef,
		) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			unsupported(CiOp::RerunFailed)
		}

		fn rerun_job(&self, _run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
			unsupported(CiOp::RerunJob)
		}

		fn trigger(
			&self,
			_request: Trigger,
		) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
			unsupported(CiOp::Trigger)
		}

		fn play_manual(&self, _run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
			unsupported(CiOp::PlayManual)
		}
	}

	#[test]
	fn a_host_can_implement_the_trait_without_boxing() {
		fn assert_host<H: CiHost>(host: &H) {
			let _ = host;
		}
		assert_host(&Nop);
	}
}
