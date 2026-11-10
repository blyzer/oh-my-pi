//! Pipelines, jobs, and the values a host returns for them.
//!
//! A [`RunRef`] names a pipeline and, when the operation is job-scoped, a
//! job. [`Trigger`] names a pipeline definition to start, not an existing run.

use std::{fmt, iter::FusedIterator, mem::size_of, sync::Arc};

use omp_core::{IntoStr, Str};
use serde::{Deserialize, Serialize};

use crate::{
	CiError, CiOp, CiProvider, CiStatus,
	error::{IdError, check_id},
};

/// Most input pairs one [`Trigger`] accepts.
pub const MAX_INPUTS: usize = 32;

/// Longest one trigger input value, in bytes.
pub const MAX_INPUT_VALUE: usize = 1024;

/// A pipeline, optionally narrowed to one job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunRef {
	/// Provider that owns the pipeline.
	pub provider: CiProvider,
	/// Pipeline id. Opaque.
	pub pipeline: Str,
	/// Job id, when the ref names a job.
	pub job:      Option<Str>,
}

const _: () = assert!(size_of::<RunRef>() <= 96, "RunRef must stay compact");

impl RunRef {
	/// Builds a ref after checking the pipeline id and the job id.
	///
	/// # Errors
	///
	/// Returns [`IdError`] when an id is empty, too long, or outside the
	/// identifier charset.
	pub fn try_new(
		provider: CiProvider,
		pipeline: impl IntoStr,
		job: Option<Str>,
	) -> Result<Self, IdError> {
		let pipeline = pipeline.into_str();
		check_id(&pipeline)?;
		if let Some(job) = &job {
			check_id(job)?;
		}
		Ok(Self { provider, pipeline, job })
	}

	/// Pipeline id when `op` does not accept a job.
	///
	/// [`CiOp::RerunFailed`] uses this. A ref that names a job returns
	/// [`CiError::UnexpectedJob`].
	pub fn pipeline_scope(&self, op: CiOp) -> Result<&str, CiError> {
		if self.job.is_some() {
			Err(CiError::UnexpectedJob { op })
		} else {
			Ok(self.pipeline.as_str())
		}
	}

	/// Job id when `op` requires a job.
	///
	/// [`CiOp::RerunJob`] and [`CiOp::PlayManual`] use this. A pipeline-only
	/// ref returns [`CiError::MissingJob`].
	pub fn job_scope(&self, op: CiOp) -> Result<&str, CiError> {
		self.job.as_deref().ok_or(CiError::MissingJob { op })
	}
}

impl fmt::Display for RunRef {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}/{}", self.provider.id, self.pipeline)?;
		if let Some(job) = &self.job {
			write!(formatter, "/{job}")?;
		}
		Ok(())
	}
}

/// One job in a pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Job {
	/// Job id, as the provider reports it.
	pub id:      Str,
	/// Job name. Empty when the provider has none.
	pub name:    Str,
	/// Current state.
	pub status:  CiStatus,
	/// Browser URL. Empty when the provider has none.
	pub web_url: Str,
}

const _: () = assert!(size_of::<Job>() <= 80, "Job must stay compact");

impl Job {
	/// A job with no browser URL.
	#[must_use]
	pub fn new(id: impl IntoStr, name: impl IntoStr, status: CiStatus) -> Self {
		Self { id: id.into_str(), name: name.into_str(), status, web_url: Str::new_static("") }
	}

	/// Sets the browser URL.
	#[must_use]
	pub fn with_web_url(mut self, url: impl IntoStr) -> Self {
		self.web_url = url.into_str();
		self
	}
}

/// One pipeline and the jobs the provider reported with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pipeline {
	/// Provider that owns the pipeline.
	pub provider: CiProvider,
	/// Pipeline id. Opaque.
	pub id:       Str,
	/// Pipeline name. Empty when the provider has none.
	pub name:     Str,
	/// Current state.
	pub status:   CiStatus,
	/// Browser URL. Empty when the provider has none.
	pub web_url:  Str,
	/// Revision the pipeline ran. Absent when the provider has none.
	pub revision: Option<Str>,
	/// Jobs in provider order.
	pub jobs:     Arc<[Job]>,
}

const _: () = assert!(size_of::<Pipeline>() <= 160, "Pipeline must stay compact");

impl Pipeline {
	/// A pipeline with no browser URL, no revision, and the given jobs.
	#[must_use]
	pub fn new(
		provider: CiProvider,
		id: impl IntoStr,
		name: impl IntoStr,
		status: CiStatus,
		jobs: impl IntoIterator<Item = Job>,
	) -> Self {
		Self {
			provider,
			id: id.into_str(),
			name: name.into_str(),
			status,
			web_url: Str::new_static(""),
			revision: None,
			jobs: jobs.into_iter().collect(),
		}
	}

	/// Sets the browser URL.
	#[must_use]
	pub fn with_web_url(mut self, url: impl IntoStr) -> Self {
		self.web_url = url.into_str();
		self
	}

	/// Sets the revision.
	#[must_use]
	pub fn with_revision(mut self, revision: impl IntoStr) -> Self {
		self.revision = Some(revision.into_str());
		self
	}

	/// Every job, in provider order.
	#[must_use]
	pub fn as_slice(&self) -> &[Job] {
		&self.jobs
	}
}

/// Jobs in `status`, in provider order.
pub fn jobs_with_status(
	pipeline: &Pipeline,
	status: CiStatus,
) -> impl DoubleEndedIterator<Item = &Job> + FusedIterator + Clone + '_ {
	pipeline.jobs.iter().filter(move |job| job.status == status)
}

/// One artifact a pipeline or job published.
///
/// The bytes stay with the host. This type is the metadata a caller needs
/// to ask for them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Artifact {
	/// Artifact name, as the provider reports it.
	pub name: Str,
	/// Size in bytes.
	pub size: u64,
	/// Retrieval URL. Empty when the provider has none.
	pub url:  Str,
}

const _: () = assert!(size_of::<Artifact>() <= 64, "Artifact must stay compact");

impl Artifact {
	/// An artifact with no retrieval URL.
	#[must_use]
	pub fn new(name: impl IntoStr, size: u64) -> Self {
		Self { name: name.into_str(), size, url: Str::new_static("") }
	}

	/// Sets the retrieval URL.
	#[must_use]
	pub fn with_url(mut self, url: impl IntoStr) -> Self {
		self.url = url.into_str();
		self
	}
}

/// Artifacts reported for one pipeline or job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPage {
	/// Pipeline or job the artifacts belong to.
	pub run:       RunRef,
	/// Artifacts in provider order.
	pub artifacts: Arc<[Artifact]>,
}

const _: () = assert!(size_of::<ArtifactPage>() <= 112, "ArtifactPage must stay compact");

impl ArtifactPage {
	/// Collects `artifacts` into one shared slice.
	#[must_use]
	pub fn new(run: RunRef, artifacts: impl IntoIterator<Item = Artifact>) -> Self {
		Self { run, artifacts: artifacts.into_iter().collect() }
	}

	/// Every artifact, in provider order.
	#[must_use]
	pub fn as_slice(&self) -> &[Artifact] {
		&self.artifacts
	}
}

/// One page of log text.
///
/// The host returns a page. A caller that needs more asks again. This crate
/// does not open a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogPage {
	/// Pipeline or job the text belongs to.
	pub run:      RunRef,
	/// Log text. The contract does not split lines.
	pub text:     Str,
	/// Whether the host reports this page as the end of the log.
	pub complete: bool,
}

const _: () = assert!(size_of::<LogPage>() <= 128, "LogPage must stay compact");

impl LogPage {
	/// A log page for `run`.
	#[must_use]
	pub fn new(run: RunRef, text: impl IntoStr, complete: bool) -> Self {
		Self { run, text: text.into_str(), complete }
	}
}

/// A request to start a pipeline.
///
/// `pipeline` is the definition to start, not an existing run id. `inputs`
/// is empty when the caller passes none. The host decides whether it needs
/// any.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Trigger {
	/// Provider that owns the definition.
	pub provider: CiProvider,
	/// Pipeline definition to start. Opaque.
	pub pipeline: Str,
	/// Input pairs, in caller order.
	pub inputs:   Arc<[(Str, Str)]>,
}

const _: () = assert!(size_of::<Trigger>() <= 64, "Trigger must stay compact");

impl Trigger {
	/// Builds a trigger after checking the definition id and each input.
	///
	/// # Errors
	///
	/// Returns [`CiError::Id`] when the definition id or an input key is not
	/// an identifier. Returns [`CiError::TooManyInputs`] above
	/// [`MAX_INPUTS`]. Returns [`CiError::InputTooLong`] or
	/// [`CiError::InputInvalid`] when a value is too long or contains a
	/// control character.
	pub fn try_new(
		provider: CiProvider,
		pipeline: impl IntoStr,
		inputs: impl IntoIterator<Item = (Str, Str)>,
	) -> Result<Self, CiError> {
		let pipeline = pipeline.into_str();
		check_id(&pipeline)?;
		let mut collected = Vec::new();
		for (key, value) in inputs {
			if collected.len() == MAX_INPUTS {
				return Err(CiError::TooManyInputs { len: collected.len() + 1, max: MAX_INPUTS });
			}
			check_id(&key)?;
			check_input_value(&value)?;
			collected.push((key, value));
		}
		Ok(Self { provider, pipeline, inputs: collected.into() })
	}
}

fn check_input_value(text: &str) -> Result<(), CiError> {
	if text.len() > MAX_INPUT_VALUE {
		return Err(CiError::InputTooLong { len: text.len(), max: MAX_INPUT_VALUE });
	}
	if text.bytes().any(|byte| byte.is_ascii_control()) {
		return Err(CiError::InputInvalid);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use omp_core::Str;

	use super::*;
	use crate::{CiError, CiOp, CiProvider, CiStatus};

	fn provider() -> CiProvider {
		CiProvider::try_new("ci.example").unwrap()
	}

	#[test]
	fn scope_helpers_split_pipeline_and_job_operations() {
		let pipeline = RunRef::try_new(provider(), "9", None).unwrap();
		assert_eq!(pipeline.pipeline_scope(CiOp::RerunFailed).unwrap(), "9");
		assert_eq!(
			pipeline.job_scope(CiOp::RerunJob),
			Err(CiError::MissingJob { op: CiOp::RerunJob })
		);
		assert_eq!(pipeline.to_string(), "ci.example/9");
		let job = RunRef::try_new(provider(), "9", Some(Str::new("3"))).unwrap();
		assert_eq!(
			job.pipeline_scope(CiOp::RerunFailed),
			Err(CiError::UnexpectedJob { op: CiOp::RerunFailed })
		);
		assert_eq!(job.job_scope(CiOp::PlayManual).unwrap(), "3");
		assert_eq!(job.to_string(), "ci.example/9/3");
	}

	#[test]
	fn failed_jobs_keep_provider_order() {
		let pipeline = Pipeline::new(provider(), "9", "build", CiStatus::Failure, [
			Job::new("1", "lint", CiStatus::Success),
			Job::new("2", "test", CiStatus::Failure),
			Job::new("3", "pack", CiStatus::Failure),
		]);
		let failed: Vec<&str> = jobs_with_status(&pipeline, CiStatus::Failure)
			.map(|job| job.id.as_str())
			.collect();
		assert_eq!(failed, ["2", "3"]);
		assert_eq!(pipeline.as_slice().len(), 3);
	}

	#[test]
	fn trigger_caps_inputs_and_rejects_a_control_value() {
		let trigger =
			Trigger::try_new(provider(), "nightly", [(Str::new("branch"), Str::new("main"))]).unwrap();
		assert_eq!(trigger.inputs.len(), 1);
		assert_eq!(trigger.inputs[0].1.as_str(), "main");
		let too_many =
			(0..=MAX_INPUTS).map(|index| (Str::new(index.to_string()), Str::new_static("1")));
		assert!(matches!(
			Trigger::try_new(provider(), "nightly", too_many),
			Err(CiError::TooManyInputs { max: MAX_INPUTS, .. })
		));
		assert_eq!(
			Trigger::try_new(provider(), "nightly", [(Str::new("k"), Str::new("a\nb"))]),
			Err(CiError::InputInvalid)
		);
	}
}
