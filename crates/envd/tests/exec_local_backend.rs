//! `omp-exec-local` drives envd's concrete `ExecHost` through the `omp-exec`
//! contract (ADR 0049 D3.1). The local backend crate does not depend on envd;
//! this test is the in-process proof that the implementor satisfies it.

use std::{sync::Arc, time::Duration};

use omp_adw::{ApprovalScope, NetworkScope, Posture, WriteScope};
use omp_core::Str;
use omp_envd::exec::ExecHost as EnvironmentHost;
use omp_exec::{
	BackendId, Eligibility, ExecutionBackend as _, ExecutionEvent, ExecutionId, ExecutionRequest,
	ExecutionRun as _, Intent, NeedSet, Outcome, OutputSpec, Platform, PlatformRequirement,
	ResourceRequirements, ToolchainId,
};
use omp_exec_local::{Config, LocalBackend};

#[tokio::test]
async fn exec_host_run_maps_output_and_exit() {
	let root = tempfile::tempdir().expect("temporary root");
	let backend = LocalBackend::new(Config {
		id:       BackendId::new_static("local"),
		root:     root.path().to_path_buf(),
		host:     EnvironmentHost::new(),
		platform: Platform::LINUX_X86_64,
		enforced: Posture {
			write:    WriteScope::WorkspaceWrite,
			network:  NetworkScope::Unrestricted,
			approval: ApprovalScope::Yolo,
		},
	});
	let request = ExecutionRequest {
		id:          ExecutionId::new_static("local-test"),
		intent:      Intent::Test,
		argv:        Arc::new([Str::new_static("printf"), Str::new_static("local-ok")]),
		cwd:         Str::empty(),
		env:         Arc::new([]),
		inputs:      None,
		outputs:     OutputSpec::Logs,
		toolchain:   ToolchainId::new_static("local"),
		caches:      Arc::new([]),
		platform:    PlatformRequirement {
			os:            Platform::LINUX_X86_64.os,
			arch:          Platform::LINUX_X86_64.arch,
			needs_network: false,
			needs:         NeedSet::new(),
		},
		requirement: Default::default(),
		resources:   ResourceRequirements::default(),
		eligibility: Eligibility::LocalPreferred,
		timeout:     Duration::from_secs(10),
	};
	let mut run = backend.start(request).await.expect("local start");
	let mut output = Vec::new();
	loop {
		match run.next_event().await.expect("local event") {
			ExecutionEvent::Output(frame) => output.extend_from_slice(&frame.bytes),
			ExecutionEvent::Settled(Ok(result)) => {
				assert_eq!(result.outcome, Outcome::Exited { code: 0 });
				break;
			},
			ExecutionEvent::Settled(Err(error)) => panic!("local execution failed: {error}"),
			_ => {},
		}
	}
	assert_eq!(output, b"local-ok");
}
