//! Detached external terminals follow the native journal shape and preserve
//! their CAS source.

use std::{sync::Arc, time::Duration};

use omp_agent::{
	CancelTree, DispatchPolicy, Dispatcher, ExternalDispatchEvent, ExternalDispatchRequest,
	ExternalDispatchStream, ExternalToolExecutor, ToolCancellation,
};
use omp_core::Str;
use omp_journal::blob::BlobStore;
use omp_session::{ComponentRegistry, Session};
use omp_tool::{
	ArtifactLifetime, Claims, ExpectedArtifact, JobKind, JobMetadata, JobOwner, JobRef,
	OutputProjection, Part, Precedence, Presentation, ToolTerminal,
};

mod support;

struct DetachedExternal(JobRef);

impl ExternalToolExecutor for DetachedExternal {
	fn invoke(&self, request: ExternalDispatchRequest) -> ExternalDispatchStream {
		let job = self.0.clone();
		let bytes =
			serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(job.clone())).expect("descriptor");
		let source_artifact = request.blobs.put(&bytes).expect("artifact");
		Box::pin(futures::stream::iter([ExternalDispatchEvent::DetachedProjected {
			job,
			parts: vec![Part::Text { text: Str::new_static("detached job external-job") }],
			source_artifact,
			projection: OutputProjection {
				request:      request.output_request,
				source_bytes: bytes.len() as u64,
				inline_bytes: bytes.len() as u64,
				omitted:      false,
				artifact:     None,
			},
		}]))
	}
}

#[tokio::test]
async fn detached_external_result_keeps_job_identity_source_artifact_and_replay() {
	let directory = tempfile::tempdir().expect("tempdir");
	let path = directory.path().join("detached.oms");
	let mut tools = omp_tool::Registry::new();
	tools
		.register_worker(support::tool_spec("worker", 1), Presentation::Slot, Claims {
			precedence: Precedence::CORE,
			claimant:   Str::new_static("detached-test"),
			replaces:   None,
		})
		.expect("worker");
	let identity = tools.resolved_identity("worker").expect("identity");
	let job = JobRef {
		id:       Str::new_static("external-job"),
		owner:    JobOwner::NamedProcess { name: Str::new_static("process-a"), generation: 7 },
		metadata: Arc::new(JobMetadata::running(JobKind::Shell, Str::new_static("command"), 123)),
		artifact: ExpectedArtifact {
			description: Str::new_static("settlement"),
			media_type:  Some(Str::new_static("application/json")),
			lifetime:    ArtifactLifetime::Session,
		},
	};
	let dispatcher = Dispatcher::new(
		Arc::new(tools),
		DispatchPolicy::new(BlobStore::open(directory.path()).expect("CAS")),
	)
	.with_external_executor(Arc::new(DetachedExternal(job.clone())));
	let mut session = Session::create(&path, ComponentRegistry::standard()).expect("session");
	session.begin_turn().expect("turn");
	session.user("test", Vec::new()).expect("user");
	let (entry, args) = support::call(&mut session, &identity, "detached-call");
	let report = tokio::time::timeout(
		Duration::from_secs(2),
		dispatcher.dispatch(
			&mut session,
			support::request(
				entry,
				identity,
				args,
				ToolCancellation::ReadOnly(CancelTree::new().begin_turn().read_only_tool()),
				false,
			),
		),
	)
	.await
	.expect("bounded dispatch")
	.expect("dispatch");
	assert_eq!(report.detached.as_ref(), Some(&job));
	assert!(!report.is_error);
	assert_eq!(support::result_text(&session, "detached-call"), ["detached job external-job"]);
	let entries = support::journal_entries(&path);
	let result = entries
		.iter()
		.find(|entry| entry.kind.name.as_str() == omp_journal::kind::TOOL_RESULT)
		.expect("terminal entry");
	let data: serde_json::Value = serde_json::from_str(result.data.as_str()).expect("terminal data");
	assert_eq!(data["outcome"]["kind"], "detached");
	assert_eq!(data["outcome"]["id"], "external-job");
	assert_eq!(
		serde_json::from_value::<JobRef>(data["outcome"]["job"].clone()).expect("durable job"),
		job
	);
	assert!(!data["source_blob"].is_null(), "original descriptor remains addressable");
	let bytes = serde_json::to_vec(&ToolTerminal::<(), ()>::Detached(job)).expect("descriptor");
	let source =
		omp_journal::blob::BlobRef { hash: omp_core::Hash32::sum(&bytes), size: bytes.len() as u64 };
	assert_eq!(
		session
			.blobs()
			.get(&source)
			.expect("descriptor bytes")
			.as_ref(),
		bytes
	);
	let live = session.dom().snapshot();
	drop(session);
	let reopened = Session::open(&path, ComponentRegistry::standard()).expect("reopen");
	assert_eq!(reopened.dom().snapshot(), live);
	assert_eq!(support::result_text(&reopened, "detached-call"), ["detached job external-job"]);
}
