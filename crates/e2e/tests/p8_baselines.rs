//! Executable P8 checks for the non-gating performance baseline recorder.

use std::{collections::BTreeMap, fs, time::Duration};

#[cfg(unix)]
use omp_e2e::baseline::record_matrix;
use omp_e2e::baseline::{
	BaselineMetrics, DEFAULT_REPETITIONS, DaemonState, LOOP_THROUGHPUT_FLOOR, MATRIX_SCHEMA_VERSION,
	MatrixArtifact, MatrixCell, MatrixMetadata, MatrixMetric, MatrixSelection,
	REPLAY_THROUGHPUT_FLOOR, Summary, Transport, Workload, duration_rate, is_gross_regression,
	is_replay_gross_regression, measure, slowdown_ratio, write_matrix, write_metrics,
};

#[test]
fn ratio_math_and_zero_duration_are_guarded() {
	assert_eq!(duration_rate(500, Duration::from_millis(250)).unwrap(), 2_000.0);
	assert_eq!(slowdown_ratio(5_000.0, 1_000.0).unwrap(), 5.0);
	assert!(duration_rate(1, Duration::ZERO).is_err());
	assert!(duration_rate(0, Duration::from_nanos(1)).is_err());
	assert!(slowdown_ratio(1.0, 0.0).is_err());
	assert!(slowdown_ratio(f64::INFINITY, 1.0).is_err());
}

#[test]
fn gross_regression_is_throughput_below_the_absolute_floor() {
	assert_eq!(LOOP_THROUGHPUT_FLOOR, 10_000.0);
	assert!(!is_gross_regression(LOOP_THROUGHPUT_FLOOR), "a rate at the floor passes");
	assert!(is_gross_regression(LOOP_THROUGHPUT_FLOOR - 1.0));
	// The two sides of the history the floor has to separate, both recorded
	// on the self-hosted M1 Pro in the dev profile.
	assert!(is_gross_regression(189.18), "one F_FULLFSYNC per delta");
	assert!(!is_gross_regression(121_984.22), "coalesced stream deltas");
}

#[test]
fn replay_gross_regression_is_throughput_below_its_absolute_floor() {
	assert_eq!(REPLAY_THROUGHPUT_FLOOR, 1_000.0);
	assert!(!is_replay_gross_regression(REPLAY_THROUGHPUT_FLOOR), "a rate at the floor passes");
	assert!(is_replay_gross_regression(REPLAY_THROUGHPUT_FLOOR - 1.0));
	// The two sides the floor separates, both from the 30-rewind, 123-entry
	// workload: the dev-profile replay recorded 3 516 entries/s on the
	// self-hosted M1 Pro (2026-10-24), and the loaded-host pathology the
	// floor exists to catch replayed the same shape in 1.974 s, which is
	// 62 entries/s (the CI failure that ungated the replay-law test).
	assert!(!is_replay_gross_regression(3_516.0), "the recorded healthy replay passes");
	assert!(is_replay_gross_regression(62.0), "the loaded-host pathology is caught");
}

#[tokio::test]
async fn artifact_schema_is_stable_and_frame_metric_is_record_only() {
	let metrics = Box::pin(measure(128, 256, 2, 8))
		.await
		.expect("bounded baseline measurement");
	assert_eq!(metrics.schema_version, 3);
	assert_eq!(metrics.frame.token_count, 128);
	assert_eq!(metrics.frame.sample_count, 128);
	assert!(metrics.frame.p95_frame_ns > 0);
	assert_eq!(metrics.r#loop.tokens_per_sample, 256);
	assert_eq!(metrics.r#loop.sample_count, 2);
	assert_eq!(metrics.r#loop.min_tokens_per_second, LOOP_THROUGHPUT_FLOOR);
	assert_eq!(
		metrics.r#loop.gross_regression,
		is_gross_regression(metrics.r#loop.full_tokens_per_second)
	);

	let scratch = tempfile::tempdir().expect("artifact scratch directory");
	let artifact = scratch.path().join("nested/p8.json");
	write_metrics(&artifact, &metrics).expect("write metrics");
	let encoded = fs::read_to_string(&artifact).expect("read metrics");
	let value: serde_json::Value = serde_json::from_str(&encoded).expect("valid JSON");
	let root = value.as_object().expect("metrics object");
	assert_eq!(root.keys().collect::<Vec<_>>(), ["schema_version", "frame", "loop", "replay"]);
	assert_eq!(
		root["replay"]
			.as_object()
			.unwrap()
			.keys()
			.collect::<Vec<_>>(),
		[
			"rewind_depth",
			"entry_count",
			"replay_duration_ns",
			"entries_per_second",
			"min_entries_per_second",
			"gross_regression",
		]
	);
	assert_eq!(metrics.replay.rewind_depth, 8);
	assert!(metrics.replay.entry_count > 8, "each rewind journals more than one entry");
	assert_eq!(metrics.replay.min_entries_per_second, REPLAY_THROUGHPUT_FLOOR);
	assert_eq!(
		metrics.replay.gross_regression,
		is_replay_gross_regression(metrics.replay.entries_per_second)
	);
	assert_eq!(
		root["frame"]
			.as_object()
			.unwrap()
			.keys()
			.collect::<Vec<_>>(),
		["token_count", "sample_count", "p95_frame_ns"]
	);
	assert_eq!(root["loop"].as_object().unwrap().keys().collect::<Vec<_>>(), [
		"tokens_per_sample",
		"sample_count",
		"raw_duration_ns",
		"full_loop_duration_ns",
		"raw_tokens_per_second",
		"full_tokens_per_second",
		"slowdown_ratio",
		"min_tokens_per_second",
		"gross_regression",
	]);
	let decoded: BaselineMetrics = serde_json::from_str(&encoded).expect("stable schema decodes");
	assert_eq!(decoded.schema_version, metrics.schema_version);
	assert_eq!(decoded.frame, metrics.frame);
	assert_eq!(decoded.r#loop.tokens_per_sample, metrics.r#loop.tokens_per_sample);
	assert_eq!(decoded.r#loop.sample_count, metrics.r#loop.sample_count);
	assert_eq!(decoded.r#loop.raw_duration_ns, metrics.r#loop.raw_duration_ns);
	assert_eq!(decoded.r#loop.full_loop_duration_ns, metrics.r#loop.full_loop_duration_ns);
	assert_eq!(decoded.r#loop.min_tokens_per_second, metrics.r#loop.min_tokens_per_second);
	assert_eq!(decoded.r#loop.gross_regression, metrics.r#loop.gross_regression);
	for (actual, expected) in [
		(decoded.r#loop.raw_tokens_per_second, metrics.r#loop.raw_tokens_per_second),
		(decoded.r#loop.full_tokens_per_second, metrics.r#loop.full_tokens_per_second),
		(decoded.r#loop.slowdown_ratio, metrics.r#loop.slowdown_ratio),
	] {
		assert!((actual - expected).abs() <= expected.abs() * f64::EPSILON);
	}
}

#[test]
fn summary_reports_median_nearest_rank_p95_and_mad() {
	let thirty: Vec<f64> = (1..=30).map(f64::from).collect();
	let summary = Summary::of(&thirty).expect("summary of thirty samples");
	assert_eq!(summary, Summary {
		samples: 30,
		min:     1.0,
		max:     30.0,
		p50:     15.5,
		p95:     29.0,
		mad:     7.5,
	});
	let mut reversed = thirty;
	reversed.reverse();
	assert_eq!(Summary::of(&reversed).expect("order does not matter"), summary);

	let odd = Summary::of(&[5.0, 1.0, 9.0, 3.0, 7.0]).expect("summary of five samples");
	assert_eq!((odd.p50, odd.p95, odd.mad), (5.0, 9.0, 2.0));
	let one = Summary::of(&[42.0]).expect("summary of one sample");
	assert_eq!((one.p50, one.p95, one.mad), (42.0, 42.0, 0.0));

	let mut noisy = vec![100.0; 29];
	noisy.push(10_000.0);
	let noisy = Summary::of(&noisy).expect("summary with an outlier");
	assert_eq!((noisy.p50, noisy.p95, noisy.mad, noisy.max), (100.0, 100.0, 0.0, 10_000.0));

	assert!(Summary::of(&[]).is_err(), "no samples");
	assert!(Summary::of(&[1.0, f64::NAN]).is_err(), "non-finite sample");
}

#[test]
fn matrix_selection_names_parse_and_default_to_the_complete_matrix() {
	assert_eq!("W7".parse::<Workload>().unwrap(), Workload::W7);
	assert_eq!("w9".parse::<Workload>().unwrap(), Workload::W9);
	assert!("W0".parse::<Workload>().is_err());
	assert!("W10".parse::<Workload>().is_err());
	assert_eq!("Attached".parse::<Transport>().unwrap(), Transport::Attached);
	assert!("remote".parse::<Transport>().is_err());
	assert_eq!("warm".parse::<DaemonState>().unwrap(), DaemonState::Warm);
	assert!("tepid".parse::<DaemonState>().is_err());

	let complete = MatrixSelection::complete();
	assert_eq!(DEFAULT_REPETITIONS, 30, "audit 12.3 asks for at least 30 runs");
	assert_eq!(complete.repetitions, DEFAULT_REPETITIONS);
	assert_eq!(complete.transports, [Transport::Embedded, Transport::Attached]);
	assert_eq!(complete.states, [DaemonState::Cold, DaemonState::Warm]);
	assert_eq!(complete.workloads.len(), 9);
	assert_eq!(complete.workloads.first(), Some(&Workload::W1));
	assert_eq!(complete.workloads.last(), Some(&Workload::W9));
}

fn keys(value: &serde_json::Value) -> Vec<&str> {
	value
		.as_object()
		.expect("JSON object")
		.keys()
		.map(String::as_str)
		.collect()
}

#[test]
fn matrix_artifact_schema_is_stable() {
	let artifact = MatrixArtifact {
		schema_version: MATRIX_SCHEMA_VERSION,
		metadata:       MatrixMetadata::capture(30),
		synthetic:      None,
		cells:          vec![MatrixCell {
			workload:                  Workload::W4,
			variant:                   Some("63KiB".to_owned()),
			transport:                 Transport::Attached,
			daemon_state:              DaemonState::Cold,
			repetitions:               30,
			operations_per_repetition: 1,
			metrics:                   vec![MatrixMetric {
				name:    "exec_ns".to_owned(),
				summary: Summary::of(&[1.0, 2.0, 3.0]).expect("summary"),
			}],
			facts:                     BTreeMap::from([("streamed_bytes".to_owned(), 64_512)]),
		}],
		failures:       Vec::new(),
	};
	let scratch = tempfile::tempdir().expect("artifact scratch directory");
	let path = scratch.path().join("nested/p8.json");
	write_matrix(&path, &artifact).expect("write matrix");
	assert!(
		!scratch.path().join("nested/p8.json.partial").exists(),
		"the staged file is renamed away"
	);
	let encoded = fs::read_to_string(&path).expect("read matrix");
	let value: serde_json::Value = serde_json::from_str(&encoded).expect("valid JSON");
	assert_eq!(keys(&value), ["schema_version", "metadata", "synthetic", "cells", "failures"]);
	assert_eq!(keys(&value["metadata"]), [
		"host_os",
		"host_arch",
		"host_version",
		"host_cpus",
		"commit_sha",
		"commit_dirty",
		"build_profile",
		"recorded_at_unix_ms",
		"repetitions",
		"interrupt_grace_ms",
	]);
	assert_eq!(value["metadata"]["host_os"], std::env::consts::OS);
	assert_eq!(value["metadata"]["host_arch"], std::env::consts::ARCH);
	assert_eq!(value["metadata"]["repetitions"], 30);
	assert_eq!(value["metadata"]["interrupt_grace_ms"], 150);
	assert!(!value["metadata"]["commit_sha"].as_str().unwrap().is_empty());
	let cell = &value["cells"][0];
	assert_eq!(keys(cell), [
		"workload",
		"variant",
		"transport",
		"daemon_state",
		"repetitions",
		"operations_per_repetition",
		"metrics",
		"facts",
	]);
	assert_eq!(cell["workload"], "W4");
	assert_eq!(cell["transport"], "attached");
	assert_eq!(cell["daemon_state"], "cold");
	assert_eq!(keys(&cell["metrics"][0]), ["name", "samples", "min", "max", "p50", "p95", "mad"]);
	let decoded: MatrixArtifact = serde_json::from_str(&encoded).expect("stable schema decodes");
	assert_eq!(decoded, artifact);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matrix_records_real_embedded_cells_with_verified_facts() {
	let scratch = tempfile::tempdir().expect("artifact scratch directory");
	let artifact = scratch.path().join("matrix.json");
	let selection = MatrixSelection {
		transports:  vec![Transport::Embedded],
		states:      vec![DaemonState::Warm, DaemonState::Cold],
		workloads:   vec![Workload::W9, Workload::W1],
		repetitions: 1,
	};
	let recording = record_matrix(&selection, &artifact, None)
		.await
		.expect("matrix recording");
	assert!(recording.failures.is_empty(), "{:?}", recording.failures);
	assert_eq!(recording.cells.len(), 4);
	for cell in &recording.cells {
		assert_eq!(cell.repetitions, 1);
		assert_eq!(cell.transport, Transport::Embedded);
		for metric in &cell.metrics {
			assert_eq!(metric.summary.samples, 1, "{}", metric.name);
			assert!(metric.summary.p50 > 0.0, "{} measured a real duration", metric.name);
		}
	}
	for state in DaemonState::ALL {
		let reads = recording
			.cells
			.iter()
			.find(|cell| cell.workload == Workload::W1 && cell.daemon_state == state)
			.expect("W1 cell");
		assert_eq!(reads.operations_per_repetition, 1_000);
		assert_eq!(reads.facts["bytes_per_read"], 1_024);
		let attach = recording
			.cells
			.iter()
			.find(|cell| cell.workload == Workload::W9 && cell.daemon_state == state)
			.expect("W9 cell");
		let names: Vec<&str> = attach
			.metrics
			.iter()
			.map(|metric| metric.name.as_str())
			.collect();
		assert_eq!(names, ["attach_ns", "first_request_ns", "reused_request_ns", "total_ns"]);
	}
	let written: MatrixArtifact =
		serde_json::from_str(&fs::read_to_string(&artifact).expect("read matrix"))
			.expect("decode matrix");
	assert_eq!(written, recording);
}
