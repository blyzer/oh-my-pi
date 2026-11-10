//! Two in-memory hosts prove host routing, the capability ceiling, and the
//! write gate.

use std::{future::Future, sync::Arc};

use futures::executor::block_on;
use omp_ci::{
	Artifact, ArtifactPage, Capabilities, CiError, CiHost, CiOp, CiProvider, CiStatus, IdError, Job,
	LogPage, Pipeline, RouteError, RunRef, Trigger,
};
use omp_ci_delegated::{ApprovalGate, Delegated, DenyAll, PermitAll, RegisterError};
use omp_core::Str;
use parking_lot::Mutex;

const ALL: Capabilities = Capabilities {
	rerun_failed:        true,
	rerun_job:           true,
	cancel:              true,
	trigger_with_inputs: true,
	artifacts:           true,
	log_stream:          true,
	play_manual:         true,
};

const READS: [CiOp; 3] = [CiOp::Status, CiOp::Artifacts, CiOp::LogStream];

const WRITES: [CiOp; 5] =
	[CiOp::Cancel, CiOp::RerunFailed, CiOp::RerunJob, CiOp::Trigger, CiOp::PlayManual];

#[derive(Clone)]
struct Memory {
	label:            &'static str,
	ops:              Arc<Mutex<Vec<CiOp>>>,
	capability_reads: Arc<Mutex<u32>>,
}

impl Memory {
	fn new(label: &'static str) -> Self {
		Self {
			label,
			ops: Arc::new(Mutex::new(Vec::new())),
			capability_reads: Arc::new(Mutex::new(0)),
		}
	}

	fn note(&self, op: CiOp) {
		self.ops.lock().push(op);
	}

	fn ops(&self) -> Vec<CiOp> {
		self.ops.lock().clone()
	}

	fn capability_reads(&self) -> u32 {
		*self.capability_reads.lock()
	}
}

struct Seen {
	op:       CiOp,
	provider: Str,
}

struct RecordGate {
	seen:  Arc<Mutex<Vec<Seen>>>,
	allow: bool,
}

impl RecordGate {
	fn new(allow: bool) -> (Self, Arc<Mutex<Vec<Seen>>>) {
		let seen = Arc::new(Mutex::new(Vec::new()));
		(Self { seen: Arc::clone(&seen), allow }, seen)
	}
}

impl ApprovalGate for RecordGate {
	fn permit(&self, op: CiOp, provider: &CiProvider) -> bool {
		self
			.seen
			.lock()
			.push(Seen { op, provider: provider.id.clone() });
		self.allow
	}
}

fn provider(host: &str) -> CiProvider {
	CiProvider::try_new(host).unwrap()
}

fn pipeline_run(host: &str) -> RunRef {
	RunRef::try_new(provider(host), "9", None).unwrap()
}

fn job_run(host: &str) -> RunRef {
	RunRef::try_new(provider(host), "9", Some(Str::new_static("3"))).unwrap()
}

fn trigger_of(host: &str) -> Trigger {
	let inputs: [(Str, Str); 0] = [];
	Trigger::try_new(provider(host), "nightly", inputs).unwrap()
}

fn named(owner: CiProvider, id: Str, label: &str) -> Pipeline {
	Pipeline::new(owner, id, label, CiStatus::Running, [Job::new("3", label, CiStatus::Running)])
}

impl CiHost for Memory {
	fn capabilities(
		&self,
		_provider: &CiProvider,
	) -> impl Future<Output = Result<Capabilities, CiError>> + Send {
		*self.capability_reads.lock() += 1;
		async move { Ok(ALL) }
	}

	fn status(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		self.note(CiOp::Status);
		let owner = run.provider.clone();
		let id = run.pipeline.clone();
		let label = self.label;
		async move { Ok(named(owner, id, label)) }
	}

	fn artifacts(&self, run: &RunRef) -> impl Future<Output = Result<ArtifactPage, CiError>> + Send {
		self.note(CiOp::Artifacts);
		let run = run.clone();
		let label = self.label;
		async move { Ok(ArtifactPage::new(run, [Artifact::new(label, 1)])) }
	}

	fn log_stream(&self, run: &RunRef) -> impl Future<Output = Result<LogPage, CiError>> + Send {
		self.note(CiOp::LogStream);
		let run = run.clone();
		let label = self.label;
		async move { Ok(LogPage::new(run, label, true)) }
	}

	fn cancel(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		self.note(CiOp::Cancel);
		let owner = run.provider.clone();
		let id = run.pipeline.clone();
		let label = self.label;
		async move { Ok(named(owner, id, label)) }
	}

	fn rerun_failed(&self, run: &RunRef) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		self.note(CiOp::RerunFailed);
		let owner = run.provider.clone();
		let id = run.pipeline.clone();
		let label = self.label;
		async move { Ok(named(owner, id, label)) }
	}

	fn rerun_job(&self, run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
		self.note(CiOp::RerunJob);
		let id = run.job.clone().unwrap_or_else(|| Str::new_static("3"));
		let label = self.label;
		async move { Ok(Job::new(id, label, CiStatus::Queued)) }
	}

	fn trigger(&self, request: Trigger) -> impl Future<Output = Result<Pipeline, CiError>> + Send {
		self.note(CiOp::Trigger);
		let label = self.label;
		async move { Ok(named(request.provider, request.pipeline, label)) }
	}

	fn play_manual(&self, run: &RunRef) -> impl Future<Output = Result<Job, CiError>> + Send {
		self.note(CiOp::PlayManual);
		let id = run.job.clone().unwrap_or_else(|| Str::new_static("3"));
		let label = self.label;
		async move { Ok(Job::new(id, label, CiStatus::Running)) }
	}
}

fn router_with<G: ApprovalGate>(gate: G) -> (Delegated<Memory, G>, Memory, Memory) {
	let left = Memory::new("a");
	let right = Memory::new("b");
	let mut builder = Delegated::builder(gate);
	builder.register("ci-a.example", left.clone(), ALL).unwrap();
	builder
		.register("ci-b.example", right.clone(), ALL)
		.unwrap();
	(builder.build(), left, right)
}

fn read_all(router: &impl CiHost, host: &str, label: &str) {
	let caps = block_on(router.capabilities(&provider(host))).unwrap();
	assert_eq!(caps, ALL);
	let viewed = block_on(router.status(&pipeline_run(host))).unwrap();
	assert_eq!(viewed.name.as_str(), label);
	let page = block_on(router.artifacts(&pipeline_run(host))).unwrap();
	assert_eq!(page.as_slice()[0].name.as_str(), label);
	let log = block_on(router.log_stream(&pipeline_run(host))).unwrap();
	assert_eq!(log.text.as_str(), label);
	assert!(log.complete);
}

fn write_all(router: &impl CiHost, host: &str, label: &str) {
	let canceled = block_on(router.cancel(&pipeline_run(host))).unwrap();
	assert_eq!(canceled.name.as_str(), label);
	let rerun = block_on(router.rerun_failed(&pipeline_run(host))).unwrap();
	assert_eq!(rerun.name.as_str(), label);
	let job = block_on(router.rerun_job(&job_run(host))).unwrap();
	assert_eq!(job.name.as_str(), label);
	let started = block_on(router.trigger(trigger_of(host))).unwrap();
	assert_eq!(started.name.as_str(), label);
	let played = block_on(router.play_manual(&job_run(host))).unwrap();
	assert_eq!(played.name.as_str(), label);
}

#[test]
fn requests_for_one_host_do_not_reach_the_other() {
	let (router, left, right) = router_with(PermitAll);

	read_all(&router, "ci-a.example", "a");
	write_all(&router, "ci-a.example", "a");
	assert_eq!(left.ops(), [READS.as_slice(), WRITES.as_slice()].concat());
	assert_eq!(left.capability_reads(), 1);
	assert!(right.ops().is_empty());
	assert_eq!(right.capability_reads(), 0);

	read_all(&router, "ci-b.example", "b");
	write_all(&router, "ci-b.example", "b");
	assert_eq!(left.ops().len(), READS.len() + WRITES.len());
	assert_eq!(left.capability_reads(), 1);
	assert_eq!(right.ops(), [READS.as_slice(), WRITES.as_slice()].concat());
	assert_eq!(right.capability_reads(), 1);
}

#[test]
fn unknown_host_names_the_method_operation_and_skips_the_gate() {
	let (gate, seen) = RecordGate::new(true);
	let (router, left, right) = router_with(gate);
	let missing = "Missing.Example";

	match block_on(router.capabilities(&provider(missing))).unwrap_err() {
		CiError::ProviderNotFound { provider } => assert_eq!(provider.as_str(), "missing.example"),
		other => panic!("unexpected {other:?}"),
	}
	match router
		.resolve("https://Missing.Example/pipelines/9")
		.unwrap_err()
	{
		CiError::ProviderNotFound { provider } => assert_eq!(provider.as_str(), "missing.example"),
		other => panic!("unexpected {other:?}"),
	}

	let cases = [
		block_on(router.status(&pipeline_run(missing))).unwrap_err(),
		block_on(router.artifacts(&pipeline_run(missing))).unwrap_err(),
		block_on(router.log_stream(&pipeline_run(missing))).unwrap_err(),
		block_on(router.cancel(&pipeline_run(missing))).unwrap_err(),
		block_on(router.rerun_failed(&pipeline_run(missing))).unwrap_err(),
		block_on(router.rerun_job(&job_run(missing))).unwrap_err(),
		block_on(router.trigger(trigger_of(missing))).unwrap_err(),
		block_on(router.play_manual(&job_run(missing))).unwrap_err(),
	];
	let mut ops = Vec::new();
	for err in cases {
		match err {
			CiError::Unregistered { provider, op } => {
				assert_eq!(provider.as_str(), "missing.example");
				ops.push(op);
			},
			other => panic!("unexpected {other:?}"),
		}
	}
	assert_eq!(ops, [READS.as_slice(), WRITES.as_slice()].concat());
	assert!(seen.lock().is_empty());
	assert!(left.ops().is_empty());
	assert!(right.ops().is_empty());
	assert_eq!(left.capability_reads(), 0);
	assert_eq!(right.capability_reads(), 0);
}

#[test]
fn writes_hit_the_gate_with_the_method_operation_and_reads_do_not() {
	let (gate, seen) = RecordGate::new(true);
	let (router, left, right) = router_with(gate);
	let host = "CI-A.Example";

	read_all(&router, host, "a");
	assert!(seen.lock().is_empty(), "reads must not ask the gate");
	assert_eq!(left.ops(), READS);
	assert_eq!(left.capability_reads(), 1);
	assert!(right.ops().is_empty());

	write_all(&router, host, "a");
	let recorded: Vec<_> = seen
		.lock()
		.iter()
		.map(|hit| (hit.op, hit.provider.clone()))
		.collect();
	assert_eq!(
		recorded,
		WRITES
			.into_iter()
			.map(|op| (op, Str::new("CI-A.Example")))
			.collect::<Vec<_>>()
	);
	assert_eq!(left.ops(), [READS.as_slice(), WRITES.as_slice()].concat());
	assert!(right.ops().is_empty());
	assert_eq!(right.capability_reads(), 0);
}

#[test]
fn a_refused_write_does_not_reach_the_adapter() {
	let (router, left, right) = router_with(DenyAll);
	let host = "ci-a.example";

	read_all(&router, host, "a");
	assert_eq!(left.ops(), READS);

	let refused = [
		block_on(router.cancel(&pipeline_run(host))).unwrap_err(),
		block_on(router.rerun_failed(&pipeline_run(host))).unwrap_err(),
		block_on(router.rerun_job(&job_run(host))).unwrap_err(),
		block_on(router.trigger(trigger_of(host))).unwrap_err(),
		block_on(router.play_manual(&job_run(host))).unwrap_err(),
	];
	for (err, op) in refused.into_iter().zip(WRITES) {
		match err {
			CiError::NotApproved { provider, op: got } => {
				assert_eq!(provider.as_str(), "ci-a.example");
				assert_eq!(got, op);
			},
			other => panic!("unexpected {other:?}"),
		}
	}
	assert_eq!(left.ops(), READS, "refused writes must not reach the adapter");
	assert!(right.ops().is_empty());
}

#[test]
fn registered_capabilities_bound_the_call_and_skip_the_gate() {
	let (gate, seen) = RecordGate::new(true);
	let left = Memory::new("a");
	let right = Memory::new("b");
	let mut builder = Delegated::builder(gate);
	let ceiling = Capabilities { cancel: true, ..Capabilities::none() };
	builder
		.register("ci-a.example", left.clone(), ceiling)
		.unwrap();
	builder
		.register("ci-b.example", right.clone(), ALL)
		.unwrap();
	let router = builder.build();
	let host = "ci-a.example";

	let viewed = block_on(router.status(&pipeline_run(host))).unwrap();
	assert_eq!(viewed.name.as_str(), "a");
	assert_eq!(left.capability_reads(), 0, "status must not query capabilities");

	let refused = [
		block_on(router.artifacts(&pipeline_run(host))).unwrap_err(),
		block_on(router.log_stream(&pipeline_run(host))).unwrap_err(),
		block_on(router.rerun_failed(&pipeline_run(host))).unwrap_err(),
		block_on(router.rerun_job(&job_run(host))).unwrap_err(),
		block_on(router.trigger(trigger_of(host))).unwrap_err(),
		block_on(router.play_manual(&job_run(host))).unwrap_err(),
	];
	let refused_ops = [
		CiOp::Artifacts,
		CiOp::LogStream,
		CiOp::RerunFailed,
		CiOp::RerunJob,
		CiOp::Trigger,
		CiOp::PlayManual,
	];
	for (err, op) in refused.into_iter().zip(refused_ops) {
		match err {
			CiError::NotCapable { provider, op: got } => {
				assert_eq!(provider.as_str(), "ci-a.example");
				assert_eq!(got, op);
			},
			other => panic!("unexpected {other:?}"),
		}
	}
	assert!(seen.lock().is_empty(), "a refused capability must not ask the gate");
	assert_eq!(left.ops(), [CiOp::Status]);
	assert_eq!(left.capability_reads(), 0);

	let canceled = block_on(router.cancel(&pipeline_run(host))).unwrap();
	assert_eq!(canceled.name.as_str(), "a");
	assert_eq!(seen.lock().iter().map(|hit| hit.op).collect::<Vec<_>>(), vec![CiOp::Cancel]);
	assert_eq!(left.ops(), [CiOp::Status, CiOp::Cancel]);
	assert_eq!(left.capability_reads(), 0, "a write must not query capabilities");

	let reported = block_on(router.capabilities(&provider(host))).unwrap();
	assert_eq!(reported, ALL, "capabilities forwards the adapter report");
	assert!(reported.allows(CiOp::Trigger));
	assert_eq!(left.capability_reads(), 1);
	match block_on(router.trigger(trigger_of(host))).unwrap_err() {
		CiError::NotCapable { op, .. } => assert_eq!(op, CiOp::Trigger),
		other => panic!("unexpected {other:?}"),
	}
	assert_eq!(left.ops(), [CiOp::Status, CiOp::Cancel]);
	assert!(right.ops().is_empty());
	assert_eq!(right.capability_reads(), 0);
}

#[test]
fn host_match_folds_ascii_case_and_a_details_url_uses_route() {
	let (gate, seen) = RecordGate::new(true);
	let plain = Memory::new("plain");
	let port = Memory::new("port");
	let v6 = Memory::new("v6");
	let mut builder = Delegated::builder(gate);
	builder
		.register("ci-a.example", plain.clone(), ALL)
		.unwrap();
	builder
		.register("ci-a.example:8443", port.clone(), ALL)
		.unwrap();
	builder.register("::1", v6.clone(), ALL).unwrap();
	let router = builder.build();

	let folded = block_on(router.status(&pipeline_run("CI-A.EXAMPLE"))).unwrap();
	assert_eq!(folded.name.as_str(), "plain");
	let with_port = block_on(router.status(&pipeline_run("ci-a.example:8443"))).unwrap();
	assert_eq!(with_port.name.as_str(), "port");

	let resolved = router
		.resolve("https://CI-A.Example:8443/pipelines/9/jobs/3")
		.unwrap();
	assert_eq!(resolved.id.as_str(), "ci-a.example");
	let via_url = block_on(router.status(&RunRef::try_new(resolved, "9", None).unwrap())).unwrap();
	assert_eq!(via_url.name.as_str(), "plain");

	let resolved_v6 = router.resolve("https://[::1]/pipelines/1").unwrap();
	assert_eq!(resolved_v6.id.as_str(), "::1");
	let v6_status =
		block_on(router.status(&RunRef::try_new(resolved_v6, "1", None).unwrap())).unwrap();
	assert_eq!(v6_status.name.as_str(), "v6");

	match router
		.resolve("https://missing.example/pipelines/1")
		.unwrap_err()
	{
		CiError::ProviderNotFound { provider } => assert_eq!(provider.as_str(), "missing.example"),
		other => panic!("unexpected {other:?}"),
	}
	match router.resolve("ci-a.example/pipelines/1").unwrap_err() {
		CiError::Route(RouteError::MissingScheme) => {},
		other => panic!("unexpected {other:?}"),
	}
	assert!(seen.lock().is_empty());
	assert_eq!(plain.ops(), [CiOp::Status, CiOp::Status]);
	assert_eq!(port.ops(), [CiOp::Status]);
	assert_eq!(v6.ops(), [CiOp::Status]);
}

#[test]
fn duplicate_and_invalid_hosts_are_rejected() {
	let mut builder = Delegated::builder(PermitAll);
	builder
		.register("CI-A.Example", Memory::new("a"), ALL)
		.unwrap();
	match builder
		.register("ci-a.example", Memory::new("b"), ALL)
		.unwrap_err()
	{
		RegisterError::Duplicate { host } => assert_eq!(host.as_str(), "ci-a.example"),
		other => panic!("unexpected {other:?}"),
	}
	match builder.register("", Memory::new("c"), ALL).unwrap_err() {
		RegisterError::Id(IdError::Empty) => {},
		other => panic!("unexpected {other:?}"),
	}
	match builder
		.register("bad host", Memory::new("c"), ALL)
		.unwrap_err()
	{
		RegisterError::Id(IdError::Invalid) => {},
		other => panic!("unexpected {other:?}"),
	}
	match builder.register("a/b", Memory::new("c"), ALL).unwrap_err() {
		RegisterError::Id(IdError::Invalid) => {},
		other => panic!("unexpected {other:?}"),
	}
	let _ = builder.build();
}
