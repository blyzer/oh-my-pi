//! The matrix driver: which cells to run, how each repetition starts its
//! authority, and how repetitions become a [`MatrixCell`].

use std::{collections::BTreeMap, path::Path};

use super::{
	BaselineMetrics, DaemonState, MATRIX_SCHEMA_VERSION, MatrixArtifact, MatrixCell, MatrixFailure,
	MatrixMetadata, MatrixMetric, MatrixSelection, Summary, Transport, Workload,
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
	for workload in workloads {
		for variant in Variant::of(workload) {
			for &transport in &selection.transports {
				for &state in &selection.states {
					let spec = CellSpec { workload, variant: variant.clone(), transport, state };
					match record_cell(&spec, selection.repetitions).await {
						Ok(cell) => {
							print_cell(&spec, &cell);
							recording.cells.push(cell);
						},
						Err(failure) => {
							eprintln!("{}: FAILED: {failure}", spec.name());
							recording.failures.push(MatrixFailure {
								workload,
								variant: variant.label(),
								transport,
								daemon_state: state,
								error: failure.to_string(),
							});
						},
					}
					write_matrix(artifact, &recording)?;
				}
			}
		}
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

async fn record_cell(spec: &CellSpec, repetitions: usize) -> Result<MatrixCell> {
	let plan = spec.plan();
	let samples = match spec.workload {
		Workload::W8 => {
			containment::repetitions(spec.transport, spec.state, &plan, repetitions).await?
		},
		Workload::W9 => workloads::attaches(spec.transport, spec.state, &plan, repetitions).await?,
		_ => steady(spec, &plan, repetitions).await?,
	};
	summarize(spec, samples)
}

/// Runs `repetitions` repetitions of a steady-state workload: a fresh
/// authority for each when cold, one warmed authority for all when warm.
async fn steady(spec: &CellSpec, plan: &Plan, repetitions: usize) -> Result<Vec<Sample>> {
	let mut samples = Vec::with_capacity(repetitions);
	match spec.state {
		DaemonState::Cold => {
			for _ in 0..repetitions {
				quiesce().await;
				let target = Staging::new(plan).await?.start(spec.transport).await?;
				let sample = workloads::run(spec.workload, &spec.variant, &target).await;
				let closed = target.close().await;
				samples.push(sample?);
				closed?;
			}
		},
		DaemonState::Warm => {
			let target = Staging::new(plan).await?.start(spec.transport).await?;
			let result: Result<()> = async {
				workloads::run(spec.workload, &spec.variant, &target).await?;
				for _ in 0..repetitions {
					quiesce().await;
					samples.push(workloads::run(spec.workload, &spec.variant, &target).await?);
				}
				Ok(())
			}
			.await;
			let closed = target.close().await;
			result?;
			closed?;
		},
	}
	Ok(samples)
}

fn summarize(spec: &CellSpec, samples: Vec<Sample>) -> Result<MatrixCell> {
	let first = samples.first().context("a cell recorded no repetitions")?;
	let mut metrics = Vec::with_capacity(first.metrics.len());
	for (index, (name, _)) in first.metrics.iter().enumerate() {
		let mut values = Vec::with_capacity(samples.len());
		for sample in &samples {
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
	for sample in &samples {
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
	Ok(MatrixCell {
		workload: spec.workload,
		variant: spec.variant.label(),
		transport: spec.transport,
		daemon_state: spec.state,
		repetitions: samples.len(),
		operations_per_repetition: workloads::operations(spec.workload, &spec.variant),
		metrics,
		facts,
	})
}
