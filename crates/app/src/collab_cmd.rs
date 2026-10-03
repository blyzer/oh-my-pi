//! `omp collab list|link`: local collaboration host discovery.
//!
//! Listing returns metadata only. A link travels over the host's owner-only
//! endpoint solely when a caller asks for one, and is bound to the room
//! generation the caller listed.

use std::{io::Write, path::Path};

use miette::IntoDiagnostic as _;
use omp_driver::collab::registry::{
	Access, DEFAULT_QUERY_TIMEOUT, HostSnapshot, LinkError, REGISTRY_VERSION, ResolvedLink,
	default_registry_dir, list_hosts, resolve_link,
};
use serde::Serialize;
use xutf::IntoAnsiStripped as _;

use crate::{
	cli::{CollabArgs, CollabCommand, CollabLinkArgs, CollabListArgs},
	usage_error::CliUsageError,
};

/// Versioned top-level JSON shape for `omp collab list --json`.
#[derive(Serialize)]
struct ListOutput<'a> {
	version: u32,
	hosts:   &'a [HostSnapshot],
}

/// Versioned capability response for `omp collab link --json`.
#[derive(Serialize)]
struct LinkOutput<'a> {
	version: u32,
	#[serde(flatten)]
	link:    &'a ResolvedLink,
}

/// Runs the selected discovery operation against the default registry.
pub async fn run(args: CollabArgs) -> miette::Result<()> {
	let dir = default_registry_dir().into_diagnostic()?;
	let mut stdout = std::io::stdout();
	match args
		.command
		.unwrap_or(CollabCommand::List(CollabListArgs { json: false }))
	{
		CollabCommand::List(args) => list(&dir, args.json, &mut stdout).await,
		CollabCommand::Link(args) => link(&dir, &args, &mut stdout).await,
	}
}

/// One host's text row: identity line plus a dim detail line.
fn render_host(host: &HostSnapshot, now_ms: u64, out: &mut impl Write) -> std::io::Result<()> {
	let session = match host.session_name.as_deref() {
		Some(name) => format!("{} ({})", clean(name), clean(host.session_id.as_str())),
		None => clean(host.session_id.as_str()),
	};
	let guests = host.participants.saturating_sub(1);
	let mut details = vec![
		format!("pid {}", host.pid),
		format!("gen {}", host.generation),
		host.model.as_ref().map_or_else(
			|| "no model".to_owned(),
			|model| clean(&format!("{}/{}", model.provider, model.id)),
		),
		format!(
			"started {}",
			omp_tui::components::relative_age(now_ms.saturating_sub(host.started_at))
		),
		format!("{guests} {}", if guests == 1 { "guest" } else { "guests" }),
		host.access.to_string(),
		format!(
			"relay {}",
			if host.relay_connected {
				"connected"
			} else {
				"reconnecting"
			}
		),
	];
	if host.input_required {
		details.push("input required".to_owned());
	}
	details.push(if host.busy { "working" } else { "idle" }.to_owned());
	writeln!(out)?;
	writeln!(out, "{}  {}  {}", host.instance_id, session, clean(host.cwd.as_str()))?;
	writeln!(out, "  {}", details.join(" · "))
}

/// Session names and paths come from other processes and may carry tabs,
/// newlines, or escape bytes; keep each on one clean line.
fn clean(text: &str) -> String {
	text
		.to_owned()
		.into_ansi_stripped()
		.chars()
		.filter(|character| !character.is_control())
		.collect()
}

fn now_ms() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

async fn list(dir: &Path, json: bool, out: &mut impl Write) -> miette::Result<()> {
	let hosts = list_hosts(dir, DEFAULT_QUERY_TIMEOUT)
		.await
		.into_diagnostic()?;
	if json {
		let output = ListOutput { version: REGISTRY_VERSION, hosts: &hosts };
		serde_json::to_writer_pretty(&mut *out, &output).into_diagnostic()?;
		writeln!(out).into_diagnostic()?;
		return Ok(());
	}
	if hosts.is_empty() {
		writeln!(out, "No active collab hosts.").into_diagnostic()?;
		return Ok(());
	}
	writeln!(
		out,
		"{} active collab {}",
		hosts.len(),
		if hosts.len() == 1 { "host" } else { "hosts" }
	)
	.into_diagnostic()?;
	let now = now_ms();
	for host in &hosts {
		render_host(host, now, out).into_diagnostic()?;
	}
	writeln!(out, "Get a link: omp collab link <instanceId|pid> [--view]").into_diagnostic()
}

async fn link(dir: &Path, args: &CollabLinkArgs, out: &mut impl Write) -> miette::Result<()> {
	let access = if args.view {
		Access::View
	} else {
		Access::Control
	};
	// Resolution failures other than a broken registry are operator-facing
	// answers (unknown host, stale room), not internal faults.
	let link = match resolve_link(dir, args.selector.as_str(), access, DEFAULT_QUERY_TIMEOUT).await {
		Ok(link) => link,
		Err(LinkError::Registry(error)) => return Err(error).into_diagnostic(),
		Err(other) => return Err(CliUsageError::startup(other.to_string()).into()),
	};
	if args.json {
		let output = LinkOutput { version: REGISTRY_VERSION, link: &link };
		serde_json::to_writer_pretty(&mut *out, &output).into_diagnostic()?;
		writeln!(out).into_diagnostic()
	} else {
		writeln!(out, "{}", link.url).into_diagnostic()
	}
}

#[cfg(all(test, unix))]
mod tests {
	use std::sync::Arc;

	use omp_core::Str;
	use omp_driver::collab::registry::{HostRegistrySource, ModelRef, Publication};

	use super::*;

	struct Host {
		access: Access,
	}

	impl HostRegistrySource for Host {
		fn snapshot(&self) -> Option<HostSnapshot> {
			Some(HostSnapshot {
				instance_id:     Str::new("0123456789abcdef"),
				generation:      3,
				pid:             std::process::id(),
				session_id:      Str::new("01SESSION"),
				session_name:    Some(Str::new("evil\x1b[31m\nname")),
				cwd:             Str::new("/work"),
				model:           Some(ModelRef { provider: Str::new("p"), id: Str::new("m") }),
				started_at:      now_ms(),
				participants:    3,
				relay_connected: true,
				input_required:  true,
				busy:            true,
				access:          self.access,
			})
		}

		fn link(&self, access: Access) -> Option<Str> {
			Some(Str::new(format!("room.{access}")))
		}
	}

	fn fixture(access: Access) -> (tempfile::TempDir, std::path::PathBuf, Publication) {
		let root = tempfile::tempdir().expect("scratch");
		let dir = root.path().join("hosts");
		let publication = Publication::publish(&dir, "0123456789abcdef", Arc::new(Host { access }))
			.expect("publish");
		(root, dir, publication)
	}

	#[tokio::test]
	async fn list_renders_one_clean_line_per_field_and_json_is_versioned() {
		let (_root, dir, _publication) = fixture(Access::Control);
		let mut text = Vec::new();
		list(&dir, false, &mut text).await.expect("list");
		let text = String::from_utf8(text).expect("utf8");
		assert!(text.starts_with("1 active collab host\n"), "{text}");
		assert!(text.contains("0123456789abcdef  evilname (01SESSION)  /work"), "{text}");
		for detail in
			["gen 3", "p/m", "2 guests", "control", "relay connected", "input required", "working"]
		{
			assert!(text.contains(detail), "{detail} missing from {text}");
		}
		assert!(!text.contains('\u{1b}'), "escape bytes never reach the terminal");

		let mut json = Vec::new();
		list(&dir, true, &mut json).await.expect("json");
		let value: serde_json::Value = serde_json::from_slice(&json).expect("json");
		assert_eq!(value["version"], 1);
		assert_eq!(value["hosts"][0]["instanceId"], "0123456789abcdef");
		assert_eq!(value["hosts"][0]["access"], "control");
	}

	#[tokio::test]
	async fn empty_registry_says_so() {
		let root = tempfile::tempdir().expect("scratch");
		let mut text = Vec::new();
		list(&root.path().join("none"), false, &mut text)
			.await
			.expect("list");
		assert_eq!(String::from_utf8(text).unwrap(), "No active collab hosts.\n");
	}

	#[tokio::test]
	async fn link_prints_the_url_or_a_typed_usage_error() {
		let (_root, dir, _publication) = fixture(Access::View);
		let request = |selector: &str, view: bool, json: bool| CollabLinkArgs {
			selector: Str::new(selector),
			view,
			json,
		};
		let mut text = Vec::new();
		link(&dir, &request("0123456789abcdef", true, false), &mut text)
			.await
			.expect("view link");
		assert_eq!(String::from_utf8(text).unwrap(), "room.view\n");

		let mut json = Vec::new();
		link(&dir, &request("0123456789abcdef", true, true), &mut json)
			.await
			.expect("json link");
		let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
		assert_eq!(value["url"], "room.view");
		assert_eq!(value["generation"], 3);
		assert_eq!(value["version"], 1);

		let refused = link(&dir, &request("0123456789abcdef", false, false), &mut Vec::new())
			.await
			.expect_err("view-only host refuses control");
		assert!(
			refused
				.to_string()
				.contains("does not publish control access"),
			"{refused}"
		);
		let missing = link(&dir, &request("nope", true, false), &mut Vec::new())
			.await
			.expect_err("unknown host");
		assert!(
			missing
				.to_string()
				.contains("no active collab host matches nope"),
			"{missing}"
		);
	}
}
