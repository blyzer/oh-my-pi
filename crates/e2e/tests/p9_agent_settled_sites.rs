//! Python `agent_settled` hooks answer through the environment authority's hook
//! gate when the authority is opened through `open_project` and
//! `open_session_host`, not only `open_local` (see `p9_agent_settled`).

#![cfg(unix)]

use omp_agent::{AgentSettled, LifecycleHooks};
use omp_e2e::{
	Result,
	support::{InProcessEnvHarness, PythonExtension, Scratch},
};
use serde_json::json;

const SOURCE: &str = r#"
import omp


@omp.hook("agent_settled")
async def settled(event, ctx):
    return omp.agents.Continue(prompt="one more")
"#;

static EXTENSION: PythonExtension =
	PythonExtension::new("p9_settled_sites_extension", SOURCE, &[]).trusting();

fn payload() -> serde_json::Value {
	json!({
		"submission_id": "s1",
		"reason": "stop",
		"committed_turns": 1,
		"last_stop": "stop",
		"pending_jobs": [],
		"continuations_used": 0,
		"incomplete_todos": [],
	})
}

async fn assert_continues(harness: InProcessEnvHarness) {
	let hooks = LifecycleHooks::new(harness.server().admission_gate());
	let outcome = hooks.agent_settled(payload()).await;
	assert_eq!(outcome.decision, AgentSettled::Continue, "the extension asked for one more turn");
	assert!(
		format!("{:?}", outcome.context).contains("one more"),
		"the prompt is the continuation context: {:?}",
		outcome.context
	);
	harness.shutdown().await;
}

#[tokio::test]
async fn python_agent_settled_continues_through_open_project() -> Result<()> {
	let harness =
		InProcessEnvHarness::spawn_project_with_extension(Scratch::new()?, &EXTENSION).await?;
	assert_continues(harness).await;
	Ok(())
}

#[tokio::test]
async fn python_agent_settled_continues_through_open_session_host() -> Result<()> {
	let harness =
		InProcessEnvHarness::spawn_session_host_with_extension(Scratch::new()?, &EXTENSION).await?;
	assert_continues(harness).await;
	Ok(())
}
