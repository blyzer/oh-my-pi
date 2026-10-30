//! The matrix driver: which cells to run, how each repetition starts its
//! authority, and how repetitions become a [`MatrixCell`].

use std::{collections::BTreeMap, path::Path};

use super::{
	BaselineMetrics, DaemonState, MATRIX_SCHEMA_VERSION, MatrixArtifact, MatrixCell, MatrixFailure,
	MatrixMetadata, MatrixMetric, MatrixSample, MatrixSelection, Summary, Transport, Workload,
	census::quiesce,
	containment,
	topology::Staging,
	workloads::{self, Plan, Sample, Variant},
	write_matrix,
};
use crate::{Context as _, Result, error};

struct CellSpec {
	workload:  Workload,
	variant:   Variant,
	transport: Transport,
	state:     DaemonState,
}

impl CellSpec {
	fn name(&self) -> String {
		let variant = self
			.variant
			.label()
			.map_or_else(String::new, |label| format!(" {label}"));
		format!("{}{variant} {} {}", self.workload, self.transport, self.state)
	}

	fn plan(&self) -> Plan {
		workloads::plan(self.workload, &self.variant, self.transport)
	}
}

/// Records the selected cells of the matrix into `artifact`, rewriting the
/// file after every cell so an interrupted run leaves a readable recording.
///
/// A cell that fails, including a W8 correctness gate, is recorded in the
/// artifact's `failures` and does not stop the remaining cells.
pub async fn record_matrix(
	selection: &MatrixSelection,
	artifact: &Path,
	synthetic: Option<BaselineMetrics>,
	variant_index: Option<usize>,
) -> Result<MatrixArtifact> {
	if selection.repetitions == 0 {
		return Err(error("a recording needs at least one repetition per cell"));
	}
	let mut recording = MatrixArtifact {
		schema_version: MATRIX_SCHEMA_VERSION,
		metadata: MatrixMetadata::capture(selection.repetitions),
		synthetic,
		cells: Vec::new(),
		failures: Vec::new(),
	};
	write_matrix(artifact, &recording)?;
	let mut workloads = selection.workloads.clone();
	workloads.sort_unstable();
	workloads.dedup();
	// The correctness gate goes first: a broken containment fails fast, and its
	// in-process cancellation proof is timed before earlier workloads' stopped
	// authorities have left residue in the recorder process.
	workloads.sort_by_key(|workload| *workload != Workload::W8);
	let mut matched_variant = variant_index.is_none();
	for workload in workloads {
		for (index, variant) in Variant::of(workload).into_iter().enumerate() {
			if variant_index.is_some_and(|only| only != index) {
				continue;
			}
			matched_variant = true;
			for &transport in &selection.transports {
				for &state in &selection.states {
					let spec = CellSpec { workload, variant: variant.clone(), transport, state };
					match record_cell(&spec, selection.repetitions).await {
						Ok(cell) => {
							print_cell(&spec, &cell);
							recording.cells.push(cell);
						},
						Err(failure) => {
							eprintln!("{}: FAILED: {}", spec.name(), &failure.error);
							recording.failures.push(MatrixFailure {
								workload,
								variant: variant.label(),
								transport,
								daemon_state: state,
								error: failure.error,

								samples: failure.samples,
								failed_repetition: failure.failed_repetition,
							});
						},
					}
					write_matrix(artifact, &recording)?;
				}
			}
		}
	}
	if !matched_variant {
		return Err(error("--variant-index selects no variant for the workload"));
	}
	Ok(recording)
}

fn print_cell(spec: &CellSpec, cell: &MatrixCell) {
	let headline = cell.metrics.first().map_or_else(String::new, |metric| {
		format!(
			"{} p50 {:.3} ms, p95 {:.3} ms, mad {:.3} ms",
			metric.name,
			metric.summary.p50 / 1e6,
			metric.summary.p95 / 1e6,
			metric.summary.mad / 1e6
		)
	});
	println!("{}: {} repetitions, {headline}", spec.name(), cell.repetitions);
}

struct CellFailure {
	samples:           Vec<MatrixSample>,
	failed_repetition: Option<usize>,
	error:             String,
}

impl CellFailure {
	fn from_error(error: crate::Error) -> Self {
		Self {
			samples:           Vec::new(),
			failed_repetition: None,
			error:             error.to_string(),
		}
	}

	fn from_partial(failure: workloads::PartialFailure) -> Self {
		Self {
			samples:           raw_samples(&failure.samples),
			failed_repetition: failure.failed_repetition,
			error:             failure.error.to_string(),
		}
	}

	fn from_samples(
		samples: Vec<Sample>,
		failed_repetition: Option<usize>,
		error: crate::Error,
	) -> Self {
		Self { samples: raw_samples(&samples), failed_repetition, error: error.to_string() }
	}
}

async fn record_cell(
	spec: &CellSpec,
	repetitions: usize,
) -> std::result::Result<MatrixCell, CellFailure> {
	let plan = spec.plan();
	let samples = match spec.workload {
		Workload::W8 => {
			let result = containment::repetitions(spec.transport, spec.state, &plan, repetitions)
				.await
				.map_err(CellFailure::from_error)?;
			if let Some(failure) = result.failure {
				return Err(CellFailure {
					samples:           raw_samples(&result.samples),
					failed_repetition: failure.repetition,
					error:             failure.error.to_string(),
				});
			}
			result.samples
		},
		Workload::W9 => workloads::attaches(spec.transport, spec.state, &plan, repetitions)
			.await
			.map_err(CellFailure::from_partial)?,
		_ => steady(spec, &plan, repetitions)
			.await
			.map_err(CellFailure::from_partial)?,
	};
	let cell =
		summarize(spec, &samples).map_err(|error| CellFailure::from_samples(samples, None, error))?;
	Ok(cell)
}
/// Runs `repetitions` repetitions of a steady-state workload: a fresh
/// authority for each when cold, one warmed authority for all when warm.
/// Runs repetitions of a steady-state workload: a fresh
/// authority for each when cold, one warmed authority for all when warm.
async fn steady(
	spec: &CellSpec,
	plan: &Plan,
	repetitions: usize,
) -> std::result::Result<Vec<Sample>, workloads::PartialFailure> {
	let mut samples = Vec::with_capacity(repetitions);
	match spec.state {
		DaemonState::Cold => {
			for index in 0..repetitions {
				quiesce().await;
				let staging = match Staging::new(plan).await {
					Ok(staging) => staging,
					Err(error) => {
						return Err(workloads::partial_failure(samples, Some(index + 1), error));
					},
				};
				let target = match staging.start(spec.transport).await {
					Ok(target) => target,
					Err(error) => {
						return Err(workloads::partial_failure(samples, Some(index + 1), error));
					},
				};
				let sample = workloads::run(spec.workload, &spec.variant, &target).await;
				let closed = target.close().await;
				match sample {
					Err(error) => {
						return Err(workloads::partial_failure(samples, Some(index + 1), error));
					},
					Ok(sample) => samples.push(sample),
				}
				if let Err(error) = closed {
					return Err(workloads::partial_failure(samples, Some(index + 1), error));
				}
			}
		},
		DaemonState::Warm => {
			let staging = match Staging::new(plan).await {
				Ok(staging) => staging,
				Err(error) => return Err(workloads::partial_failure(samples, None, error)),
			};
			let target = match staging.start(spec.transport).await {
				Ok(target) => target,
				Err(error) => return Err(workloads::partial_failure(samples, None, error)),
			};
			if let Err(error) = workloads::run(spec.workload, &spec.variant, &target).await {
				let _ = target.close().await;
				return Err(workloads::partial_failure(samples, None, error));
			}
			for index in 0..repetitions {
				quiesce().await;
				match workloads::run(spec.workload, &spec.variant, &target).await {
					Ok(sample) => samples.push(sample),
					Err(error) => {
						let _ = target.close().await;
						return Err(workloads::partial_failure(samples, Some(index + 1), error));
					},
				}
			}
			if let Err(error) = target.close().await {
				return Err(workloads::partial_failure(samples, None, error));
			}
		},
	}
	Ok(samples)
}

fn raw_samples(samples: &[Sample]) -> Vec<MatrixSample> {
	samples
		.iter()
		.map(|sample| MatrixSample {
			metrics: sample
				.metrics
				.iter()
				.map(|(name, value)| ((*name).to_owned(), *value))
				.collect(),
			facts:   sample
				.facts
				.iter()
				.map(|(name, value)| ((*name).to_owned(), *value))
				.collect(),
		})
		.collect()
}

fn summarize(spec: &CellSpec, samples: &[Sample]) -> Result<MatrixCell> {
	let first = samples.first().context("a cell recorded no repetitions")?;
	let mut metrics = Vec::with_capacity(first.metrics.len());
	for (index, (name, _)) in first.metrics.iter().enumerate() {
		let mut values = Vec::with_capacity(samples.len());
		for sample in samples {
			match sample.metrics.get(index) {
				Some((other, value)) if other == name => values.push(*value),
				_ => return Err(error(format!("repetitions disagree on metric {name}"))),
			}
		}
		metrics.push(MatrixMetric { name: (*name).to_owned(), summary: Summary::of(&values)? });
	}
	let facts: BTreeMap<String, u64> = first
		.facts
		.iter()
		.map(|(name, value)| ((*name).to_owned(), *value))
		.collect();
	for sample in samples {
		let observed: BTreeMap<String, u64> = sample
			.facts
			.iter()
			.map(|(name, value)| ((*name).to_owned(), *value))
			.collect();
		if observed != facts {
			return Err(error(format!(
				"repetitions disagree on verified facts: {facts:?} against {observed:?}"
			)));
		}
	}
	let raw_samples = raw_samples(samples);
	Ok(MatrixCell {
		workload: spec.workload,
		variant: spec.variant.label(),
		transport: spec.transport,
		daemon_state: spec.state,
		repetitions: samples.len(),
		operations_per_repetition: workloads::operations(spec.workload, &spec.variant),
		metrics,
		samples: raw_samples,
		facts,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn recorder_preserves_partial_workload_failure() {
		let failure = CellFailure::from_partial(workloads::PartialFailure {
			samples:           vec![Sample {
				metrics: vec![("elapsed_ns", 17.0)],
				facts:   vec![("completed", 1)],
			}],
			failed_repetition: Some(2),
			error:             error("repetition failed"),
		});

		assert_eq!(failure.failed_repetition, Some(2));
		assert_eq!(failure.samples.len(), 1);
		assert_eq!(failure.samples[0].metrics.get("elapsed_ns"), Some(&17.0));
		assert_eq!(failure.samples[0].facts.get("completed"), Some(&1));
	}
}
