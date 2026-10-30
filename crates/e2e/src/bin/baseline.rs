//! Executable P8 performance recorder entry point.
//!
//! Measurement and artifact logic lives in [`omp_e2e::baseline`]; this bin
//! parses the matrix selection, records the synthetic kernel measurements and
//! the audit's W1 through W9 matrix, and writes the artifact.
//!
//! Each cell is recorded in its own child process. An environment authority
//! holds its SQLite and directory handles for the life of the process that
//! opened it, so a single-process matrix exhausts a 16384 descriptor ceiling
//! partway through W7; one child per cell lets the kernel reclaim them at exit.

use std::{
	env,
	ffi::OsString,
	fs,
	path::{Path, PathBuf},
	process::Command,
	str::FromStr,
};

use omp_e2e::{
	Context as _, Result,
	baseline::{
		DEFAULT_REPETITIONS, DaemonState, MatrixSelection, Transport, Workload, isolate_user_roots,
		measure,
	},
	error,
};

const DEFAULT_ARTIFACT: &str = "target/e2e-artifacts/p8-baselines.json";
const DEFAULT_FRAME_TOKENS: usize = 2_048;
const DEFAULT_LOOP_TOKENS: usize = 8_192;
const DEFAULT_SAMPLES: usize = 5;
const DEFAULT_REWIND_DEPTH: usize = 30;

const USAGE: &str =
	"usage: baseline [--samples N] [--mode embedded|attached|all] [--daemon-state cold|warm|all] \
	 [--workload W1..W9|all] [--artifact PATH] [--variant-index N] [--isolated]\n--mode, \
	 --daemon-state and --workload take comma-separated lists and may repeat. By default every \
	 mode, daemon state and workload is recorded with 30 samples per cell into \
	 target/e2e-artifacts/p8-baselines.json, one child process per cell. --variant-index selects \
	 one of a workload's parameters and --isolated records in this process; together they are how \
	 the parent asks a child for one cell.";

struct Options {
	selection:     MatrixSelection,
	artifact:      PathBuf,
	/// Index into the workload's variants when recording exactly one cell.
	variant_index: Option<usize>,
	/// Record in this process instead of spawning one child per cell.
	isolated:      bool,
}

fn flag_value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<String> {
	args
		.next()
		.with_context(|| format!("{flag} requires a value"))?
		.into_string()
		// lintx-allow: error-format CLI boundary renders OS argument conversion.
		.map_err(|value| error(format!("{flag} value {value:?} is not UTF-8")))
}

fn select<T>(into: &mut Vec<T>, value: &str, every: &[T]) -> Result<()>
where
	T: FromStr<Err = omp_e2e::Error> + Copy + PartialEq,
{
	let chosen: Vec<T> = if value.eq_ignore_ascii_case("all") {
		every.to_vec()
	} else {
		value
			.split(',')
			.map(|item| item.trim().parse())
			.collect::<Result<_>>()?
	};
	for item in chosen {
		if !into.contains(&item) {
			into.push(item);
		}
	}
	Ok(())
}

fn parse_options(mut args: impl Iterator<Item = OsString>) -> Result<Option<Options>> {
	let mut repetitions = DEFAULT_REPETITIONS;
	let mut transports = Vec::new();
	let mut states = Vec::new();
	let mut workloads = Vec::new();
	let mut artifact = PathBuf::from(DEFAULT_ARTIFACT);
	let mut variant_index = None;
	let mut isolated = false;
	while let Some(flag) = args.next() {
		let flag = flag
			.into_string()
			// lintx-allow: error-format CLI boundary renders OS argument conversion.
			.map_err(|flag| error(format!("argument {flag:?} is not UTF-8")))?;
		match flag.as_str() {
			"--samples" => {
				repetitions = flag_value(&mut args, &flag)?
					.parse()
					.context("--samples takes a positive integer")?;
				if repetitions == 0 {
					return Err(error("--samples takes a positive integer"));
				}
			},
			"--mode" => select(&mut transports, &flag_value(&mut args, &flag)?, &Transport::ALL)?,
			"--daemon-state" => {
				select(&mut states, &flag_value(&mut args, &flag)?, &DaemonState::ALL)?;
			},
			"--workload" => select(&mut workloads, &flag_value(&mut args, &flag)?, &Workload::ALL)?,
			"--artifact" => artifact = PathBuf::from(flag_value(&mut args, &flag)?),
			"--variant-index" => {
				variant_index = Some(
					flag_value(&mut args, &flag)?
						.parse()
						.context("--variant-index takes a variant index")?,
				);
			},
			"--isolated" => isolated = true,
			"--help" | "-h" => {
				println!("{USAGE}");
				return Ok(None);
			},
			other => return Err(error(format!("unexpected argument {other:?}\n{USAGE}"))),
		}
	}
	let complete = MatrixSelection::complete();
	Ok(Some(Options {
		selection: MatrixSelection {
			transports: or_all(transports, complete.transports),
			states: or_all(states, complete.states),
			workloads: or_all(workloads, complete.workloads),
			repetitions,
		},
		artifact,
		variant_index,
		isolated,
	}))
}

fn or_all<T>(chosen: Vec<T>, every: Vec<T>) -> Vec<T> {
	if chosen.is_empty() { every } else { chosen }
}

#[cfg(unix)]
fn failure_lines(recording: &omp_e2e::baseline::MatrixArtifact) -> Vec<String> {
	recording
		.failures
		.iter()
		.map(|failure| {
			format!(
				"{} {} {} failed: {}",
				failure.workload, failure.transport, failure.daemon_state, failure.error
			)
		})
		.collect()
}

/// Every cell the selection covers, as the parent hands them to children.
///
/// The correctness gate is ordered first, matching the in-process recorder, so
/// a containment failure is recorded before later workloads run. Ordering is
/// all that buys: a failed cell never stops the remaining ones.
#[cfg(unix)]
fn cells(selection: &MatrixSelection) -> Vec<(Workload, usize, Transport, DaemonState)> {
	let mut workloads = selection.workloads.clone();
	workloads.sort_unstable();
	workloads.dedup();
	workloads.sort_by_key(|workload| *workload != Workload::W8);
	let mut cells = Vec::new();
	for workload in workloads {
		for index in 0..omp_e2e::baseline::variant_count(workload) {
			for &transport in &selection.transports {
				for &state in &selection.states {
					cells.push((workload, index, transport, state));
				}
			}
		}
	}
	cells
}

/// Records one cell in a child process and returns what it wrote.
///
/// A child that fails its cell still records that failure in its own artifact,
/// so only a child that died without writing one becomes an error here.
#[cfg(unix)]
fn record_cell(
	executable: &Path,
	options: &Options,
	cell: (Workload, usize, Transport, DaemonState),
	artifact: &Path,
) -> Result<omp_e2e::baseline::MatrixArtifact> {
	let (workload, index, transport, state) = cell;
	let status = Command::new(executable)
		.arg("--isolated")
		.arg("--samples")
		.arg(options.selection.repetitions.to_string())
		.arg("--workload")
		.arg(workload.to_string())
		.arg("--variant-index")
		.arg(index.to_string())
		.arg("--mode")
		.arg(transport.to_string())
		.arg("--daemon-state")
		.arg(state.to_string())
		.arg("--artifact")
		.arg(artifact)
		.status()
		.context("spawning the cell recorder")?;
	let encoded = fs::read_to_string(artifact)
		.with_context(|| format!("reading cell artifact {}", artifact.display()))?;
	let recorded: omp_e2e::baseline::MatrixArtifact =
		serde_json::from_str(&encoded).context("decoding a cell artifact")?;
	let expected_variant = omp_e2e::baseline::variant_label(workload, index);
	let matches_cell = recorded.cells.iter().any(|cell| {
		cell.workload == workload
			&& cell.variant == expected_variant
			&& cell.transport == transport
			&& cell.daemon_state == state
	}) || recorded.failures.iter().any(|failure| {
		failure.workload == workload
			&& failure.variant == expected_variant
			&& failure.transport == transport
			&& failure.daemon_state == state
	});
	if !matches_cell {
		return Err(error(format!(
			"cell recorder exited {status} without a matching artifact for {workload} variant \
			 {index} {transport} {state}"
		)));
	}
	if !status.success() && recorded.failures.is_empty() {
		return Err(error(format!(
			"cell recorder exited {status} without recording a failure for {workload} variant \
			 {index} {transport} {state}"
		)));
	}
	Ok(recorded)
}

#[cfg(unix)]
async fn record(
	options: &Options,
	synthetic: omp_e2e::baseline::BaselineMetrics,
) -> Result<Vec<String>> {
	let executable = env::current_exe().context("resolving the recorder executable")?;
	let mut recording = omp_e2e::baseline::MatrixArtifact {
		schema_version: omp_e2e::baseline::MATRIX_SCHEMA_VERSION,
		metadata:       omp_e2e::baseline::MatrixMetadata::capture(options.selection.repetitions),
		synthetic:      Some(synthetic),
		cells:          Vec::new(),
		failures:       Vec::new(),
	};
	omp_e2e::baseline::write_matrix(&options.artifact, &recording)?;
	for (cell_index, cell) in cells(&options.selection).into_iter().enumerate() {
		let (workload, index, transport, state) = cell;
		let scratch = options
			.artifact
			.with_extension(format!("cell-{cell_index}.json"));
		match fs::remove_file(&scratch) {
			Ok(()) => {},
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
			Err(error) => return Err(error.into()),
		}
		match record_cell(&executable, options, cell, &scratch) {
			Ok(mut recorded) => {
				recording.cells.append(&mut recorded.cells);
				recording.failures.append(&mut recorded.failures);
			},
			Err(failure) => recording.failures.push(omp_e2e::baseline::MatrixFailure {
				workload,
				variant: (omp_e2e::baseline::variant_count(workload) > 1)
					.then(|| format!("variant {index}")),
				transport,
				daemon_state: state,
				// lintx-allow: error-format artifact boundary renders child failure.
				error: failure.to_string(),
				samples: Vec::new(),
				failed_repetition: None,
			}),
		}
		match fs::remove_file(&scratch) {
			Ok(()) => {},
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
			Err(error) => return Err(error.into()),
		}
		omp_e2e::baseline::write_matrix(&options.artifact, &recording)?;
	}
	println!("wrote {}", options.artifact.display());
	Ok(failure_lines(&recording))
}

#[cfg(unix)]
async fn record_isolated(options: &Options) -> Result<Vec<String>> {
	let recording = omp_e2e::baseline::record_matrix(
		&options.selection,
		&options.artifact,
		None,
		options.variant_index,
	)
	.await?;
	Ok(failure_lines(&recording))
}

#[cfg(not(unix))]
async fn record(_: &Options, _: omp_e2e::baseline::BaselineMetrics) -> Result<Vec<String>> {
	Err(error("the P8 matrix recorder needs a Unix host"))
}

#[cfg(not(unix))]
async fn record_isolated(_: &Options) -> Result<Vec<String>> {
	Err(error("the P8 matrix recorder needs a Unix host"))
}

fn main() -> Result<()> {
	let Some(options) = parse_options(env::args_os().skip(1))? else {
		return Ok(());
	};
	let _roots = isolate_user_roots()?;
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.context("building the async runtime")?
		.block_on(run(options))
}

async fn run(options: Options) -> Result<()> {
	let mut problems = Vec::new();
	if options.isolated {
		problems.extend(record_isolated(&options).await?);
	} else {
		let synthetic = Box::pin(measure(
			DEFAULT_FRAME_TOKENS,
			DEFAULT_LOOP_TOKENS,
			DEFAULT_SAMPLES,
			DEFAULT_REWIND_DEPTH,
		))
		.await?;
		println!("synthetic: {}", serde_json::to_string(&synthetic)?);
		if synthetic.r#loop.gross_regression {
			problems.push(format!(
				"full-loop throughput {:.0} tokens/s is below the {:.0} tokens/s floor",
				synthetic.r#loop.full_tokens_per_second, synthetic.r#loop.min_tokens_per_second
			));
		}
		if synthetic.replay.gross_regression {
			problems.push(format!(
				"replay throughput {:.0} entries/s is below the {:.0} entries/s floor",
				synthetic.replay.entries_per_second, synthetic.replay.min_entries_per_second
			));
		}
		problems.extend(record(&options, synthetic).await?);
	}
	if problems.is_empty() {
		Ok(())
	} else {
		Err(error(problems.join("\n")))
	}
}
