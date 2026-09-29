//! `omp share`: host a session headlessly and print its collaboration links.
//!
//! The command composes the same kernel and session `omp print` and `omp rpc`
//! do, then hands them to [`omp_driver::collab::host::HeadlessRoom`], which
//! owns the relay session, the local host registry entry, and guest-mutation
//! admission. This module only presents: it prints the links and waits for a
//! process signal.

use std::{fs, sync::Arc};

use miette::{IntoDiagnostic as _, miette};
use omp_agent::{SessionShutdown, SessionStart, ShutdownReason};
use omp_collab::link::{CollabLink, DEFAULT_RELAY_URL, RelayEndpoint, WebEndpoint};
use omp_core::Str;
use omp_driver::collab::{
	host::{HeadlessRoom, RoomOptions, ServeEnd},
	observer::HostAgentBridge,
	registry::{Access, default_registry_dir},
};
use omp_proto::collab::v1::{ModelMetadata, SessionStateUpdate};
use omp_session::ExitCause;
use serde::Serialize;
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

use crate::{
	chat_cmd::{Launch, LaunchEnv},
	cli::ShareArgs,
	usage_error::CliUsageError,
};

/// One published link in both of its forms.
#[derive(Serialize)]
struct PublishedLink<'a> {
	/// Compact form, joinable with `omp join`.
	link:    &'a str,
	/// Browser deep link; credentials stay in the URL fragment.
	browser: Option<String>,
}

/// The room facts `omp share --json` prints once the room is live.
#[derive(Serialize)]
struct RoomFacts<'a> {
	session: &'a str,
	viewer:  PublishedLink<'a>,
	editor:  Option<PublishedLink<'a>>,
}

fn published<'a>(link: &'a Str) -> PublishedLink<'a> {
	let browser = CollabLink::parse(link.as_str())
		.ok()
		.map(|parsed| parsed.browser(&WebEndpoint::from_relay(parsed.relay())));
	PublishedLink { link: link.as_str(), browser }
}

fn render_facts(facts: &RoomFacts<'_>) -> String {
	use std::fmt::Write as _;
	let mut text = String::new();
	let _ = writeln!(text, "Sharing session {}", facts.session);
	let mut row = |label: &str, link: &PublishedLink<'_>| {
		let _ = writeln!(text, "{label}: {}", link.link);
		if let Some(browser) = &link.browser {
			let _ = writeln!(text, "{:width$}  {browser}", "", width = label.len());
		}
	};
	row("Viewer (read-only)", &facts.viewer);
	if let Some(editor) = &facts.editor {
		row("Editor (can prompt)", editor);
	}
	text.push_str("Join with `omp join <link>`. Press Ctrl-C to stop sharing.\n");
	text
}

/// Runs the headless host until a process signal or the relay closes the room.
pub async fn run(args: ShareArgs) -> miette::Result<()> {
	let ShareArgs { launch, relay, view, json } = args;
	if !launch.prompt.is_empty() {
		return Err(
			CliUsageError::new("omp share takes no prompt: writable guests supply the prompts").into(),
		);
	}
	if launch.from_claude || launch.from_codex {
		return Err(miette!("share does not accept interactive legacy session imports"));
	}
	let relay =
		RelayEndpoint::parse(relay.as_deref().unwrap_or(DEFAULT_RELAY_URL)).into_diagnostic()?;
	let project = fs::canonicalize(&launch.project).into_diagnostic()?;
	let ctx = Arc::new(crate::process_ctx(&project)?);
	let env = LaunchEnv::production(&project, launch.gateway.is_some())?;
	let mut launch = Launch::prepare(launch, ctx, env).await?;
	let (mut kernel, mut session) = launch.compose().await?;
	let lifecycle = kernel.lifecycle_hooks();
	report_launch_warnings(&launch).await?;
	if let Some(lifecycle) = &lifecycle {
		lifecycle
			.session_start(&SessionStart::launch(&session, &launch.project))
			.await
			.into_diagnostic()?;
	}
	let ephemeral_path = launch
		.ephemeral
		.then(|| session.journal_path().to_path_buf());
	let session_id = session
		.journal_path()
		.file_stem()
		.and_then(|value| value.to_str())
		.map_or_else(|| Str::new_static("ephemeral"), Str::new);
	let sessions_dir = match launch.sessions_dir.clone() {
		Some(dir) => dir,
		None => omp_env::project_state::directory(&launch.data_dir, &launch.project)
			.into_diagnostic()?
			.join("sessions"),
	};
	let (provider, model) = launch
		.model
		.as_str()
		.split_once('/')
		.map_or(("", launch.model.as_str()), |(provider, model)| (provider, model));
	let state = SessionStateUpdate {
		session_name: omp_chat::status_line::StatusLine::from_dom(session.dom())
			.name
			.unwrap_or_default()
			.to_string(),
		host_cwd: launch.project.to_string_lossy().into_owned(),
		model: Some(ModelMetadata {
			id:             model.to_owned(),
			name:           model.to_owned(),
			provider:       provider.to_owned(),
			context_window: 0,
		}),
		..SessionStateUpdate::default()
	};
	let options = RoomOptions {
		relay,
		session_id: session_id.clone(),
		access: if view { Access::View } else { Access::Control },
		agents: HostAgentBridge::new(Arc::clone(&launch.live_sessions), sessions_dir),
		state,
		registry_dir: default_registry_dir().ok(),
	};
	let mut room = HeadlessRoom::open(&mut session, options)
		.await
		.into_diagnostic()?;

	let facts = RoomFacts {
		session: session_id.as_str(),
		viewer:  published(room.viewer_link()),
		editor:  (!view).then(|| published(room.editor_link())),
	};
	let mut stdout = tokio::io::stdout();
	let announcement = if json {
		let mut encoded = serde_json::to_string(&facts).into_diagnostic()?;
		encoded.push('\n');
		encoded
	} else {
		render_facts(&facts)
	};
	stdout
		.write_all(announcement.as_bytes())
		.await
		.into_diagnostic()?;
	stdout.flush().await.into_diagnostic()?;

	let shutdown = CancellationToken::new();
	let signal_shutdown = shutdown.clone();
	let signal_task = tokio::spawn(async move {
		if crate::chat_cmd::process_signal().await.is_ok() {
			signal_shutdown.cancel();
		}
	});
	let served = room.serve(&mut kernel, &mut session, &shutdown).await;
	signal_task.abort();
	room.stop().await;

	let reason = match &served {
		Ok(ServeEnd::Shutdown | ServeEnd::RoomEnded) => ShutdownReason::Completed,
		Err(_) => ShutdownReason::Fatal,
	};
	session.record_exit(ExitCause::Normal).into_diagnostic()?;
	if let Some(lifecycle) = &lifecycle {
		lifecycle
			.session_shutdown(&SessionShutdown::new(&session, reason))
			.await;
	}
	drop(session);
	if let Some(path) = ephemeral_path {
		let _ = fs::remove_file(path);
	}
	match served {
		Ok(ServeEnd::Shutdown) => Ok(()),
		Ok(ServeEnd::RoomEnded) => Err(miette!("the relay closed the collaboration room")),
		Err(error) => Err(error).into_diagnostic(),
	}
}

/// Unapproved plugin servers and hooks never run without an interactive
/// operator; report each on stderr, keeping stdout for the links.
async fn report_launch_warnings(launch: &Launch) -> miette::Result<()> {
	use std::fmt::Write as _;
	let mut report = String::new();
	for warning in launch.plugin_warnings() {
		let _ = writeln!(report, "warning: {warning}");
	}
	if let Some(hint) = launch.plugin_dir_hint() {
		let _ = writeln!(report, "hint: {hint}");
	}
	if report.is_empty() {
		return Ok(());
	}
	let mut stderr = tokio::io::stderr();
	stderr
		.write_all(report.as_bytes())
		.await
		.into_diagnostic()?;
	stderr.flush().await.into_diagnostic()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn facts_render_both_links_with_browser_forms_and_view_only_hides_the_editor() {
		let viewer = Str::new("room.viewerkey");
		let editor = Str::new("room.editorkey");
		let both = RoomFacts {
			session: "01SESSION",
			viewer:  PublishedLink {
				link:    viewer.as_str(),
				browser: Some("https://web/#v".to_owned()),
			},
			editor:  Some(PublishedLink { link: editor.as_str(), browser: None }),
		};
		let text = render_facts(&both);
		assert!(text.contains("Sharing session 01SESSION"), "{text}");
		assert!(text.contains("Viewer (read-only): room.viewerkey"), "{text}");
		assert!(text.contains("https://web/#v"), "{text}");
		assert!(text.contains("Editor (can prompt): room.editorkey"), "{text}");
		let view_only = RoomFacts { editor: None, ..both };
		assert!(!render_facts(&view_only).contains("editorkey"));
		let json = serde_json::to_value(&view_only).expect("json");
		assert!(json["editor"].is_null());
		assert_eq!(json["viewer"]["link"], "room.viewerkey");
	}

	#[tokio::test]
	async fn a_positional_prompt_is_refused_before_any_session_work() {
		let mut launch = crate::cli::ChatArgs::default_interactive();
		launch.prompt = vec![Str::new_static("do a thing")];
		let error = run(ShareArgs { launch, relay: None, view: false, json: false })
			.await
			.expect_err("guests supply the prompts");
		assert!(error.to_string().contains("takes no prompt"), "{error}");
	}

	#[test]
	fn published_links_derive_browser_urls_only_from_parseable_links() {
		assert!(published(&Str::new("not a link at all")).browser.is_none());
	}
}
