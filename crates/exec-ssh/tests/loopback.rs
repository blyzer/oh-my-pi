#![cfg(target_os = "macos")]

use std::{env, sync::Arc, time::Duration};

use omp_adw::Posture;
use omp_core::{Str, project_file::Containment};
use omp_envd::ssh::{HostStore, SshService};
use omp_exec::{
	BackendId, Eligibility, ExecutionBackend, ExecutionEvent, ExecutionId, ExecutionRequest, Intent,
	NeedSet, OutputSpec, Platform, PlatformRequirement, ResourceRequirements, ToolchainId,
};

#[tokio::test]
async fn loopback_sshd_executes_a_pinned_host_key_command() {
	let hosts_path = env::var_os("OMP_SSH_TEST_HOSTS").expect("workflow hosts file");
	let hosts = HostStore::load(hosts_path.as_ref(), Containment::Unconfined).expect("hosts");
	let backend = SshBackend::new(Config {
		id:               BackendId::new_static("ssh-loopback"),
		alias:            Str::new_static("loopback"),
		service:          SshService::new(hosts),
		platform:         Platform::MACOS_AARCH64,
		enforced:         Posture::default(),
		max_output_bytes: 1024 * 1024,
	});
	let request = ExecutionRequest {
		id:          ExecutionId::new_static("ssh-loopback-test"),
		intent:      Intent::Test,
		argv:        Arc::new([Str::new_static("printf"), Str::new_static("ssh-ok")]),
		cwd:         Str::empty(),
		env:         Arc::new([]),
		inputs:      None,
		outputs:     OutputSpec::Logs,
		toolchain:   ToolchainId::new_static("ssh-loopback"),
		caches:      Arc::new([]),
		platform:    PlatformRequirement {
			os:            Platform::MACOS_AARCH64.os,
			arch:          Platform::MACOS_AARCH64.arch,
			needs_network: false,
			needs:         NeedSet::new(),
		},
		requirement: Default::default(),
		resources:   ResourceRequirements::default(),
		eligibility: Eligibility::RemoteRequired,
		timeout:     Duration::from_secs(20),
	};
	let mut run = backend.start(request).await.expect("SSH start");
	let mut output = Vec::new();
	loop {
		match run.next_event().await.expect("SSH event") {
			ExecutionEvent::Output(frame) => output.extend_from_slice(&frame.bytes),
			ExecutionEvent::Settled(Ok(result)) => {
				assert_eq!(result.outcome, omp_exec::Outcome::Exited { code: 0 });
				break;
			},
			ExecutionEvent::Settled(Err(error)) => panic!("SSH execution failed: {error}"),
			_ => {},
		}
	}
	assert_eq!(output, b"ssh-ok");
}

use omp_exec::BackendId;
