//! Executable P8 performance recorder entry point.
//!
//! Measurement and artifact logic lives in [`omp_e2e::baseline`]; this bin
//! parses the matrix selection, records the synthetic kernel measurements and
//! the audit's W1 through W9 matrix, and writes the artifact.

use std::{env, ffi::OsString, path::PathBuf, str::FromStr};

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

const USAGE: &str = "usage: baseline [--samples N] [--mode embedded|attached|all] [--daemon-state \
                     cold|warm|all] [--workload W1..W9|all] [--artifact PATH]\n--mode, \
                     --daemon-state and --workload take comma-separated lists and may repeat. By \
                     default every mode, daemon state and workload is recorded with 30 samples \
                     per cell into target/e2e-artifacts/p8-baselines.json.";

struct Options {
	selection: MatrixSelection,
	artifact:  PathBuf,
}

fn flag_value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<String> {
	args
		.next()
		.with_context(|| format!("{flag} requires a value"))?
		.into_string()
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
	while let Some(flag) = args.next() {
		let flag = flag
			.into_string()
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
	}))
}

fn or_all<T>(chosen: Vec<T>, every: Vec<T>) -> Vec<T> {
	if chosen.is_empty() { every } else { chosen }
}

#[cfg(unix)]
async fn record(
	options: &Options,
	synthetic: omp_e2e::baseline::BaselineMetrics,
) -> Result<Vec<String>> {
	let recording =
		omp_e2e::baseline::record_matrix(&options.selection, &options.artifact, Some(synthetic))
			.await?;
	println!("wrote {}", options.artifact.display());
	Ok(recording
		.failures
		.iter()
		.map(|failure| {
			format!(
				"{} {} {} failed: {}",
				failure.workload, failure.transport, failure.daemon_state, failure.error
			)
		})
		.collect())
}

#[cfg(not(unix))]
async fn record(_: &Options, _: omp_e2e::baseline::BaselineMetrics) -> Result<Vec<String>> {
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
	let synthetic = Box::pin(measure(
		DEFAULT_FRAME_TOKENS,
		DEFAULT_LOOP_TOKENS,
		DEFAULT_SAMPLES,
		DEFAULT_REWIND_DEPTH,
	))
	.await?;
	println!("synthetic: {}", serde_json::to_string(&synthetic)?);
	let mut problems = Vec::new();
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
	if problems.is_empty() {
		Ok(())
	} else {
		Err(error(problems.join("\n")))
	}
}
