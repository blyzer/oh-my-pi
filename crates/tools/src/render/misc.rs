//! Native forge, CI, browser, and computer renderers.

use std::fmt::Write as _;

#[cfg(feature = "cap-ci")]
use omp_ci::CiOp;
use omp_core::{Str, sf};
#[cfg(feature = "cap-forge")]
use omp_forge::ForgeOp;
use omp_tool::{CallOutcome, ToolIdentity, render::RenderFold};
use serde_json::Value;

use super::{live_view, view::El};
#[cfg(feature = "cap-ci")]
use crate::ci::{Fault as CiFault, Payload as CiPayload, Update as CiUpdate};
#[cfg(feature = "cap-forge")]
use crate::forge::{Fault as ForgeFault, Payload as ForgePayload, Update as ForgeUpdate};
use crate::{
	browser::{
		Action as BrowserAction, Fault as BrowserFault, Payload as BrowserPayload,
		Update as BrowserUpdate,
	},
	computer::{Fault as ComputerFault, Payload as ComputerPayload, Update as ComputerUpdate},
	gallery::RendererGalleryFixture,
	view,
};

#[cfg(feature = "cap-forge")]
#[derive(Default)]
pub(super) struct ForgeState {
	op:       Option<ForgeOp>,
	repo:     Option<Str>,
	query:    Option<Str>,
	progress: Option<Str>,
}

#[cfg(feature = "cap-forge")]
pub(super) struct ForgeRenderer;

#[cfg(feature = "cap-forge")]
impl RenderFold for ForgeRenderer {
	type Outcome = CallOutcome<ForgePayload, ForgeFault>;
	type State = ForgeState;
	type Update = ForgeUpdate;

	fn fold(&self, _state: &mut Self::State, _update: Self::Update) {}

	fn fold_args(&self, state: &mut Self::State, args: &omp_core::slopjson::Value, _complete: bool) {
		state.op = args
			.get("op")
			.and_then(|value| value.deserialize_into::<ForgeOp>().ok());
		state.repo = args
			.get("repo")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
		state.query = args
			.get("query")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
	}

	fn view(&self, state: &Self::State, outcome: Option<&Self::Outcome>) -> Option<Str> {
		match outcome {
			None => Some(render_forge_live(state).into()),
			Some(CallOutcome::Ok(payload)) => Some(render_forge_payload(state, payload).into()),
			Some(CallOutcome::Faulted(fault)) => Some(render_forge_fault(state, fault).into()),
			Some(CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. }) => None,
		}
	}
}

#[cfg(feature = "cap-forge")]
fn render_forge_live(state: &ForgeState) -> El {
	let action = state
		.op
		.map_or_else(|| Str::new_static("forge"), |op| sf!("{op}"));
	view! {
		<row gap=1>
			<spinner/>
			<text bold>{action}</text>
			if let Some(repo) = state.repo.as_deref() {
				<text fg=accent>{"["}{repo}{"]"}</text>
			}
			if let Some(query) = state.query.as_deref() {
				<text fg=muted truncate>{query}</text>
			}
			if let Some(progress) = state.progress.as_deref() {
				<text fg=muted truncate>{progress}</text>
			}
		</row>
	}
}

#[cfg(feature = "cap-forge")]
fn render_forge_payload(state: &ForgeState, payload: &ForgePayload) -> El {
	if matches!(
		payload.op,
		ForgeOp::SearchChangeRequests
			| ForgeOp::SearchIssues
			| ForgeOp::SearchCode
			| ForgeOp::SearchCommits
			| ForgeOp::SearchRepos
	) {
		return render_forge_search(state, payload);
	}

	let content = (payload.op == ForgeOp::FileRead).then_some(payload.output.as_str());
	view! {
		<col gap=0>
			{forge_header(state, payload)}
			if let Some(content) = content {
				<pre max-rows=40 overflow="lines">{content}</pre>
			} else {
				{json_view(&payload.result, 3, 10, 100)}
			}
		</col>
	}
}

#[cfg(feature = "cap-forge")]
fn render_forge_search(state: &ForgeState, payload: &ForgePayload) -> El {
	let hits = payload.result.get("hits").and_then(Value::as_array);
	let count = hits.map_or(0, |hits| hits.len() as u64);
	view! {
		<col gap=0>
			{forge_header(state, payload)}
			if let Some(hits) = hits {
				if hits.is_empty() {
					<text fg=muted>{"No results"}</text>
				} else {
					<table max-rows=20 overflow="results">
						for hit in hits {
							{forge_search_row(hit)}
						}
					</table>
				}
			}
			<row gap=1><text fg=muted>{sf!("{count} results")}</text></row>
		</col>
	}
}

#[cfg(feature = "cap-forge")]
fn forge_header(state: &ForgeState, payload: &ForgePayload) -> El {
	let repo = state
		.repo
		.as_deref()
		.or_else(|| payload.result.get("repo").and_then(Value::as_str))
		.or_else(|| payload.result.get("full_name").and_then(Value::as_str));
	view! {
		<row gap=1>
			<text bold>{sf!("forge {}", payload.op)}</text>
			if let Some(repo) = repo {
				<text fg=accent>{"["}{repo}{"]"}</text>
			}
			if let Some(query) = state.query.as_deref() {
				<text fg=muted truncate>{query}</text>
			}
		</row>
	}
}

#[cfg(feature = "cap-forge")]
fn forge_search_row(hit: &Value) -> El {
	let number = hit.get("number").and_then(Value::as_u64);
	let title = hit
		.get("title")
		.and_then(Value::as_str)
		.unwrap_or("untitled");
	let number_text = number.map_or_else(|| Str::new_static("—"), |number| sf!("#{number}"));
	view! {
		<tr>
			<td w=7><text fg=info bold>{number_text}</text></td>
			<td grow truncate><text>{title}</text></td>
		</tr>
	}
}

#[derive(Default)]
#[cfg(feature = "cap-ci")]
pub(super) struct CiState {
	op:       Option<CiOp>,
	provider: Option<Str>,
	pipeline: Option<Str>,
	watch:    bool,
	progress: Option<Str>,
}

#[cfg(feature = "cap-ci")]
pub(super) struct CiRenderer;

#[cfg(feature = "cap-ci")]
impl RenderFold for CiRenderer {
	type Outcome = CallOutcome<CiPayload, CiFault>;
	type State = CiState;
	type Update = CiUpdate;

	fn fold(&self, state: &mut Self::State, update: Self::Update) {
		state.progress = Some(update.output);
	}

	fn fold_args(&self, state: &mut Self::State, args: &omp_core::slopjson::Value, _complete: bool) {
		state.op = args
			.get("op")
			.and_then(|value| value.deserialize_into::<CiOp>().ok());
		state.provider = args
			.get("provider")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
		state.pipeline = args
			.get("pipeline")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
		state.watch = args
			.get("watch")
			.and_then(omp_core::slopjson::Value::as_bool)
			.unwrap_or(false);
	}

	fn view(&self, state: &Self::State, outcome: Option<&Self::Outcome>) -> Option<Str> {
		match outcome {
			None => Some(render_ci_live(state).into()),
			Some(CallOutcome::Ok(payload)) => Some(render_ci_payload(state, payload).into()),
			Some(CallOutcome::Faulted(fault)) => Some(render_ci_fault(state, fault).into()),
			Some(CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. }) => None,
		}
	}
}

#[cfg(feature = "cap-ci")]
fn render_ci_live(state: &CiState) -> El {
	let action = state
		.op
		.map_or_else(|| Str::new_static("ci"), |op| sf!("{op}"));
	view! {
		<row gap=1>
			<spinner/>
			<text bold>{action}</text>
			if let Some(provider) = state.provider.as_deref() {
				<text fg=accent>{provider}</text>
			}
			if let Some(pipeline) = state.pipeline.as_deref() {
				<text fg=muted>{pipeline}</text>
			}
			if state.watch {
				<text fg=muted>{"watch"}</text>
			}
			if let Some(progress) = state.progress.as_deref() {
				<text fg=muted truncate>{progress}</text>
			}
		</row>
	}
}

#[cfg(feature = "cap-ci")]
fn render_ci_payload(state: &CiState, payload: &CiPayload) -> El {
	view! {
		<col gap=0>
			<row gap=1>
				<text bold>{sf!("ci {}", payload.op)}</text>
				if let Some(provider) = state.provider.as_deref() {
					<text fg=accent>{provider}</text>
				}
				if let Some(pipeline) = state.pipeline.as_deref() {
					<text fg=muted>{pipeline}</text>
				}
			</row>
			<pre max-rows=40 overflow="lines">{payload.output.as_str()}</pre>
		</col>
	}
}

#[cfg(feature = "cap-ci")]
fn render_ci_fault(state: &CiState, fault: &CiFault) -> El {
	let action = state
		.op
		.map_or_else(|| Str::new_static("ci"), |op| sf!("{op}"));
	view! {
		<col gap=0>
			<row gap=1>
				<text bold fg=err>{action}</text>
				if let Some(provider) = state.provider.as_deref() {
					<text fg=muted>{provider}</text>
				}
			</row>
			<callout kind="error">{&fault.message}</callout>
		</col>
	}
}

#[cfg(feature = "cap-forge")]
fn render_forge_fault(state: &ForgeState, fault: &ForgeFault) -> El {
	let action = state
		.op
		.map_or_else(|| Str::new_static("forge"), |op| sf!("{op}"));
	view! {
		<col gap=0>
			<row gap=1>
				<text bold fg=err>{action}</text>
				if let Some(repo) = state.repo.as_deref() {
					<text fg=muted>{"["}{repo}{"]"}</text>
				}
			</row>
			<callout kind="error">{&fault.message}</callout>
		</col>
	}
}

#[derive(Default)]
pub(super) struct BrowserState {
	action: Option<BrowserAction>,
	name:   Option<Str>,
	url:    Option<Str>,
	code:   Option<Str>,
	status: Option<Str>,
}

pub(super) struct BrowserRenderer;

impl RenderFold for BrowserRenderer {
	type Outcome = CallOutcome<BrowserPayload, BrowserFault>;
	type State = BrowserState;
	type Update = BrowserUpdate;

	fn fold(&self, state: &mut Self::State, update: Self::Update) {
		match update {
			BrowserUpdate::Started { name, action, browser } => {
				state.name = Some(name);
				state.action = Some(action);
				state.status = Some(browser);
			},
			BrowserUpdate::Helper { operation } => state.status = Some(operation),
			BrowserUpdate::Artifact { uri, .. } => state.status = Some(uri),
		}
	}

	fn fold_args(&self, state: &mut Self::State, args: &omp_core::slopjson::Value, _complete: bool) {
		state.action = args
			.get("action")
			.and_then(|value| value.deserialize_into::<BrowserAction>().ok());
		state.name = args
			.get("name")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
		state.url = args
			.get("url")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
		state.code = args
			.get("code")
			.and_then(omp_core::slopjson::Value::as_str)
			.map(Str::new);
	}

	fn view(&self, state: &Self::State, outcome: Option<&Self::Outcome>) -> Option<Str> {
		match outcome {
			None => Some(render_browser_live(state).into()),
			Some(CallOutcome::Ok(payload)) => Some(render_browser_payload(state, payload).into()),
			Some(CallOutcome::Faulted(fault)) => Some(render_browser_fault(state, fault).into()),
			Some(CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. }) => None,
		}
	}
}

fn render_browser_live(state: &BrowserState) -> El {
	let Some(action) = state.action else {
		return live_view("browser", "waiting for a browser action");
	};
	view! {
		<col gap=0>
			{browser_header(
				action,
				state.name.as_deref().unwrap_or("main"),
				state.url.as_deref(),
				None,
				true,
			)}
			if action == BrowserAction::Run {
				if let Some(code) = state.code.as_deref().filter(|code| !code.is_empty()) {
					<text bold>{"Code"}</text>
					<pre max-rows=16 overflow="lines">{code}</pre>
				}
			}
			if let Some(status) = state.status.as_deref() {
				<text fg=muted>{status}</text>
			}
		</col>
	}
}

fn render_browser_payload(state: &BrowserState, payload: &BrowserPayload) -> El {
	view! {
		<col gap=0>
			{browser_header(
				payload.action,
				&payload.name,
				payload.url.as_deref().or(state.url.as_deref()),
				payload.title.as_deref(),
				false,
			)}
			if payload.action == BrowserAction::Run {
				if let Some(code) = state.code.as_deref().filter(|code| !code.is_empty()) {
					<text bold>{"Code"}</text>
					<pre max-rows=16 overflow="lines">{code}</pre>
				}
				if let Some(result) = payload.result.as_ref() {
					<text bold>{"Output"}</text>
					{browser_output(result)}
				}
			} else if let Some(result) = payload.result.as_ref() {
				{json_view(result, 3, 6, 100)}
			}
			if !payload.artifacts.is_empty() {
				<text bold>{"Artifacts"}</text>
				for artifact in &payload.artifacts {
					<fact label="Artifact"><text fg=accent>{&artifact.uri}</text></fact>
				}
			}
		</col>
	}
}

fn render_browser_fault(state: &BrowserState, fault: &BrowserFault) -> El {
	view! {
		<col gap=0>
			if let Some(action) = state.action {
				{browser_header(
					action,
					state.name.as_deref().unwrap_or("main"),
					state.url.as_deref(),
					None,
					false,
				)}
				if action == BrowserAction::Run {
					if let Some(code) = state.code.as_deref().filter(|code| !code.is_empty()) {
						<text bold>{"Code"}</text>
						<pre max-rows=16 overflow="lines">{code}</pre>
					}
				}
			} else {
				<text bold fg=err>{"Browser action failed"}</text>
			}
			<callout kind="error">{&fault.message}</callout>
		</col>
	}
}

fn browser_header(
	action: BrowserAction,
	name: &str,
	url: Option<&str>,
	title: Option<&str>,
	live: bool,
) -> El {
	view! {
		<row gap=1>
			if live {
				<spinner/>
			}
			<text bold>{sf!(r#"{action} tab "{name}""#)}</text>
			if let Some(title) = title {
				<text truncate>{title}</text>
			}
			if let Some(url) = url {
				<text fg=muted truncate>{url}</text>
			}
			<text fg=secondary>{"headless"}</text>
		</row>
	}
}

fn browser_output(result: &Value) -> El {
	let Some(object) = result.as_object() else {
		return view! { <col gap=0>{labeled_value("return", result)}</col> };
	};
	let displays = object
		.get("display_outputs")
		.or_else(|| object.get("display"));
	let returned = object.get("return_value").or_else(|| object.get("return"));
	if displays.is_none() && returned.is_none() {
		return view! { <col gap=0>{labeled_value("return", result)}</col> };
	}

	view! {
		<col gap=0>
			if let Some(displays) = displays {
				if let Some(values) = displays.as_array() {
					for value in values {
						{labeled_value("display", value)}
					}
				} else {
					{labeled_value("display", displays)}
				}
			}
			if let Some(returned) = returned {
				{labeled_value("return", returned)}
			}
		</col>
	}
}

fn labeled_value(label: &'static str, value: &Value) -> El {
	view! {
		<fact label={label}>{json_view(value, 4, 10, 100)}</fact>
	}
}

#[derive(Default)]
pub(super) struct ComputerState {
	action:  Str,
	code:    Str,
	summary: Str,
}

pub(super) struct ComputerRenderer;

impl RenderFold for ComputerRenderer {
	type Outcome = CallOutcome<ComputerPayload, ComputerFault>;
	type State = ComputerState;
	type Update = ComputerUpdate;

	fn fold(&self, state: &mut Self::State, update: Self::Update) {
		match update {
			ComputerUpdate::Started { action } => {
				if state.summary.is_empty() {
					state.summary = Str::new(<&'static str>::from(action));
				}
			},
			ComputerUpdate::Operation { operation } => {
				state.summary = Str::new(<&'static str>::from(operation));
			},
			ComputerUpdate::Artifact { .. } => {},
		}
	}

	fn fold_args(&self, state: &mut Self::State, args: &omp_core::slopjson::Value, _complete: bool) {
		state.action = args
			.get("action")
			.and_then(omp_core::slopjson::Value::as_str)
			.map_or_else(|| Str::new_static("run"), Str::new);
		state.code = args
			.get("code")
			.and_then(omp_core::slopjson::Value::as_str)
			.map_or_else(Str::default, Str::new);
		state.summary = computer_arg_summary(args);
	}

	fn view(&self, state: &Self::State, outcome: Option<&Self::Outcome>) -> Option<Str> {
		match outcome {
			None => Some(render_computer_live(state).into()),
			Some(CallOutcome::Ok(payload)) => Some(render_computer_payload(state, payload).into()),
			Some(CallOutcome::Faulted(fault)) => Some(render_computer_fault(state, fault).into()),
			Some(CallOutcome::ArgsRejected(_) | CallOutcome::Aborted { .. }) => None,
		}
	}
}

fn computer_arg_summary(args: &omp_core::slopjson::Value) -> Str {
	let mut summary = String::new();
	if let Some(action) = args
		.get("action")
		.and_then(omp_core::slopjson::Value::as_str)
		.filter(|action| *action != "run")
	{
		summary.push_str(action);
	}
	if args
		.get("read_only")
		.and_then(omp_core::slopjson::Value::as_bool)
		.unwrap_or(false)
	{
		summary.push_str("read-only");
	}
	if let Some(timeout) = args
		.get("timeout")
		.and_then(omp_core::slopjson::Value::as_f64)
	{
		if !summary.is_empty() {
			summary.push_str(" · ");
		}
		write!(summary, "{timeout}s").expect("writing to String cannot fail");
	}
	if let Some(code) = args.get("code").and_then(omp_core::slopjson::Value::as_str) {
		let first_line = code.lines().next().unwrap_or_default().trim();
		if !first_line.is_empty() {
			let mut end = first_line.len().min(80);
			while !first_line.is_char_boundary(end) {
				end -= 1;
			}
			push_summary_field(&mut summary, "", &first_line[..end]);
		}
	}
	if summary.is_empty() {
		summary.push_str("computer program");
	}
	Str::new(summary)
}

fn push_summary_field(output: &mut String, label: &str, value: &str) {
	if !output.is_empty() {
		output.push_str(" · ");
	}
	if !label.is_empty() {
		output.push_str(label);
		output.push(' ');
	}
	output.push_str(value);
}

fn render_computer_live(state: &ComputerState) -> El {
	if state.code.is_empty() && (state.action.is_empty() || state.action == "run") {
		return live_view("Computer", "waiting for code");
	}
	view! {
		<row gap=1>
			<spinner/>
			<text bold>{"Computer"}</text>
			<text fg=muted>{&state.summary}</text>
		</row>
	}
}

fn render_computer_payload(state: &ComputerState, payload: &ComputerPayload) -> El {
	let capabilities = payload
		.capabilities
		.as_ref()
		.map(|value| serde_json::to_value(value).expect("computer capabilities serialize"));
	view! {
		<col gap=0>
			<row gap=1>
				<text bold>{"Computer"}</text>
				if !state.summary.is_empty() {
					<text fg=muted>{&state.summary}</text>
				}
			</row>
			if let Some(code) = &payload.code {
				<text bold>{"Code"}</text>
				<pre max-rows=16 overflow="lines">{code}</pre>
			}
			for (index, result) in payload.results.iter().enumerate() {
				{labeled_value(if index == 0 { "result" } else { "next" }, result)}
			}
			if let Some(capabilities) = &capabilities {
				{labeled_value("capabilities", capabilities)}
			}
			for artifact in &payload.artifacts {
				<fact label="Screenshot"><text fg=accent>{&artifact.uri}</text></fact>
			}
		</col>
	}
}

fn render_computer_fault(state: &ComputerState, fault: &ComputerFault) -> El {
	view! {
		<col gap=0>
			<row gap=1>
				<text bold fg=err>{"Computer"}</text>
				if !state.summary.is_empty() {
					<text fg=muted>{&state.summary}</text>
				}
			</row>
			<callout kind="error">{&fault.message}</callout>
		</col>
	}
}

fn json_view(value: &Value, max_depth: usize, max_rows: usize, max_chars: usize) -> El {
	view! {
		<json max-depth={max_depth} max-rows={max_rows} max-chars={max_chars}>
			{value.to_string()}
		</json>
	}
}

/// Native forge, CI, browser, and computer lifecycle fixtures for the visual QA
/// gallery.
pub fn gallery_fixtures(
	#[cfg(feature = "cap-forge")] forge: ToolIdentity,
	#[cfg(feature = "cap-ci")] ci: ToolIdentity,
	browser: ToolIdentity,
	computer: ToolIdentity,
) -> Vec<RendererGalleryFixture> {
	vec![
		#[cfg(feature = "cap-forge")]
		RendererGalleryFixture {
			identity: forge,
			streaming_args: r#"{"op":"search_change_requests","query":"is:open review-requested:@me sort:up"#,
			args: r#"{"op":"search_change_requests","query":"is:open review-requested:@me sort:updated","repo":"oh-my-pi/pi","limit":10}"#,
			progress_update: None,
			success_outcome: br#"{"kind":"ok","value":{"op":"search_change_requests","result":{"hits":[{"number":1842,"title":"feat(tui): virtualized scrollback for tool output"},{"number":1839,"title":"fix(agent): retry stream on transient 529"}]},"output":"2 results","useless":false}}"#,
			error_outcome: br#"{"kind":"faulted","value":{"code":"repo_not_found","message":"forge repository pi was not found on github.com"}}"#,
		},
		#[cfg(feature = "cap-ci")]
		RendererGalleryFixture {
			identity: ci,
			streaming_args: r#"{"op":"status","provider":"github.com","pipeline":"42","watch":tr"#,
			args: r#"{"op":"status","provider":"github.com","pipeline":"42","watch":true}"#,
			progress_update: None,
			success_outcome: br#"{"kind":"ok","value":{"op":"status","result":{"status":"failure"},"output":"github.com 42 failure","useless":false}}"#,
			error_outcome: br#"{"kind":"faulted","value":{"code":"not_capable","message":"ci operation cancel is outside the capabilities of github.com"}}"#,
		},
		RendererGalleryFixture {
			identity: browser,
			streaming_args: r#"{"action":"run","name":"docs","url":"https://bun.sh/docs","code":"const obs = await tab.observe();\nconst heading = obs.elements.find(e => e.role === 'head"#,
			args: r#"{"action":"run","name":"docs","url":"https://bun.sh/docs","code":"const obs = await tab.observe();\nconst heading = obs.elements.find(e => e.role === 'heading');\ndisplay({ url: obs.url, title: obs.title, headings: obs.elements.filter(e => e.role === 'heading').length });\nreturn heading?.name ?? 'no heading found';"}"#,
			progress_update: None,
			success_outcome: br#"{"kind":"ok","value":{"action":"run","name":"docs","url":"https://bun.sh/docs","title":"Bun Documentation","result":{"display_outputs":[{"url":"https://bun.sh/docs","title":"Bun Documentation","headings":14}],"return_value":"Get started with Bun"},"artifacts":[]}}"#,
			error_outcome: br#"{"kind":"faulted","value":{"code":"browser_timeout","message":"TimeoutError: waiting for selector `aria/Sign in` failed: timeout 30000ms exceeded\n    at Tab.waitFor (browser/tab.ts:212:13)\n    at run (eval:3:7)"}}"#,
		},
		RendererGalleryFixture {
			identity: computer,
			streaming_args: r#"{"action":"run","code":"const shot = await desktop.screenshot({\"maxWidth\":1440,\"maxHeight\":9"#,
			args: r#"{"action":"run","code":"const shot = await desktop.screenshot({\"maxWidth\":1440,\"maxHeight\":900});\nassert(shot.width > 0);","read_only":true,"timeout":30}"#,
			progress_update: None,
			success_outcome: br#"{"kind":"ok","value":{"action":"run","code":"const shot = await desktop.screenshot({\"maxWidth\":1440,\"maxHeight\":900});\nassert(shot.width > 0);","results":[{"artifact":"artifact://sha256/8f9b0dd1e9c0a05d4f1d6d2ae9742d7a","width":1440,"height":900,"source_width":2880,"source_height":1800,"coordinate_space":"capture_pixels"},true],"artifacts":[{"uri":"artifact://sha256/8f9b0dd1e9c0a05d4f1d6d2ae9742d7a","mime":"image/png","visible":true,"byte_len":482193,"width":1440,"height":900,"source_width":2880,"source_height":1800,"target":"desktop"}],"capabilities":null}}"#,
			error_outcome: br#"{"kind":"faulted","value":{"code":"permission_denied","message":"required desktop permission is unavailable","operation":"capture"}}"#,
		},
	]
}

#[cfg(test)]
mod tests {
	use omp_core::Str;
	use omp_tool::{CallOutcome, Rev, ToolIdentity, render::RenderFold};

	use super::*;

	fn identity(name: &'static str) -> ToolIdentity {
		ToolIdentity { name: Str::new_static(name), rev: Rev { family: Str::default(), n: 1 } }
	}

	#[test]
	fn fixture_wire_shapes_decode_and_streaming_args_render() {
		let fixtures = gallery_fixtures(
			#[cfg(feature = "cap-forge")]
			identity("forge"),
			#[cfg(feature = "cap-ci")]
			identity("ci"),
			identity("browser"),
			identity("computer"),
		);
		for fixture in &fixtures {
			assert!(omp_core::slopjson::parse(fixture.streaming_args).is_err());
			omp_core::slopjson::parse(fixture.args).expect("committed args decode");
			assert!(fixture.progress_update.is_none());
		}

		#[cfg(feature = "cap-forge")]
		{
			let forge = fixtures
				.iter()
				.find(|fixture| fixture.identity.name.as_str() == "forge")
				.expect("forge fixture");
			let mut forge_state = ForgeState::default();
			ForgeRenderer.fold_args(
				&mut forge_state,
				&omp_core::slopjson::parse_streaming(forge.streaming_args),
				false,
			);
			assert!(
				ForgeRenderer
					.view(&forge_state, None)
					.expect("streaming forge args render")
					.contains("sort:up")
			);
			serde_json::from_slice::<CallOutcome<ForgePayload, ForgeFault>>(forge.success_outcome)
				.expect("forge success outcome decodes");
			serde_json::from_slice::<CallOutcome<ForgePayload, ForgeFault>>(forge.error_outcome)
				.expect("forge error outcome decodes");
		}
		#[cfg(feature = "cap-ci")]
		{
			let ci = fixtures
				.iter()
				.find(|fixture| fixture.identity.name.as_str() == "ci")
				.expect("ci fixture");
			let mut ci_state = CiState::default();
			CiRenderer.fold_args(
				&mut ci_state,
				&omp_core::slopjson::parse_streaming(ci.streaming_args),
				false,
			);
			assert!(
				CiRenderer
					.view(&ci_state, None)
					.expect("streaming ci args render")
					.contains("42")
			);
			serde_json::from_slice::<CallOutcome<CiPayload, CiFault>>(ci.success_outcome)
				.expect("ci success outcome decodes");
			serde_json::from_slice::<CallOutcome<CiPayload, CiFault>>(ci.error_outcome)
				.expect("ci error outcome decodes");
		}

		let browser = fixtures
			.iter()
			.find(|fixture| fixture.identity.name.as_str() == "browser")
			.expect("browser fixture");
		let mut browser_state = BrowserState::default();
		BrowserRenderer.fold_args(
			&mut browser_state,
			&omp_core::slopjson::parse_streaming(browser.streaming_args),
			false,
		);
		assert!(
			BrowserRenderer
				.view(&browser_state, None)
				.expect("streaming browser args render")
				.contains("role === 'head")
		);
		let computer = fixtures
			.iter()
			.find(|fixture| fixture.identity.name.as_str() == "computer")
			.expect("computer fixture");
		let mut computer_state = ComputerState::default();
		ComputerRenderer.fold_args(
			&mut computer_state,
			&omp_core::slopjson::parse_streaming(computer.streaming_args),
			false,
		);
		assert!(
			ComputerRenderer
				.view(&computer_state, None)
				.expect("streaming computer args render")
				.contains("desktop.screenshot")
		);

		serde_json::from_slice::<CallOutcome<BrowserPayload, BrowserFault>>(browser.success_outcome)
			.expect("browser success outcome decodes");
		serde_json::from_slice::<CallOutcome<BrowserPayload, BrowserFault>>(browser.error_outcome)
			.expect("browser error outcome decodes");
		serde_json::from_slice::<CallOutcome<ComputerPayload, ComputerFault>>(
			computer.success_outcome,
		)
		.expect("computer success outcome decodes");
		serde_json::from_slice::<CallOutcome<ComputerPayload, ComputerFault>>(computer.error_outcome)
			.expect("computer error outcome decodes");
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn forge_search_uses_aligned_rows_and_escapes_payload_text() {
		let payload = ForgePayload {
			op:      ForgeOp::SearchChangeRequests,
			result:  serde_json::json!({
				"hits": [{
					"number": 7,
					"title": "Fix <unsafe> & parsing"
				}]
			}),
			output:  Str::new_static("1 result"),
			useless: false,
		};
		let rendered = ForgeRenderer
			.view(&ForgeState::default(), Some(&CallOutcome::Ok(payload)))
			.expect("forge search renders");
		assert!(rendered.contains("<table max-rows=20 overflow=results>"));
		assert!(rendered.contains("#7"));
		assert!(rendered.contains("Fix &lt;unsafe&gt; &amp; parsing"));
		assert!(!rendered.contains("+12"));
		assert!(!rendered.contains("API requests remaining"));
		assert!(!rendered.contains("ctrl+o"));
	}

	#[cfg(feature = "cap-forge")]
	#[test]
	fn forge_file_and_structured_results_delegate_bounding_to_primitives() {
		let file = ForgePayload {
			op:      ForgeOp::FileRead,
			result:  serde_json::json!({"path": "src/lib.rs", "size": 20}),
			output:  Str::new_static("first\nsecond <third>"),
			useless: false,
		};
		let rendered = ForgeRenderer
			.view(&ForgeState::default(), Some(&CallOutcome::Ok(file)))
			.expect("forge file renders");
		assert!(rendered.contains("<pre max-rows=40 overflow=lines>"));
		assert!(rendered.contains("second &lt;third&gt;"));
		assert!(!rendered.contains("API requests remaining"));
		assert!(!rendered.contains("ctrl+o"));

		let repo = ForgePayload {
			op:      ForgeOp::RepoView,
			result:  serde_json::json!({"full_name": "oh-my-pi/pi", "private": false}),
			output:  Str::new_static("oh-my-pi/pi"),
			useless: false,
		};
		let rendered = ForgeRenderer
			.view(&ForgeState::default(), Some(&CallOutcome::Ok(repo)))
			.expect("forge structured result renders");
		assert!(
			rendered.contains("<json max-depth=3 max-rows=10 max-chars=100>"),
			"rendered: {rendered}"
		);
		assert!(rendered.contains("\"full_name\":\"oh-my-pi/pi\""), "rendered: {rendered}");
		assert!(rendered.contains("\"private\":false"), "rendered: {rendered}");
	}

	#[test]
	fn browser_failure_retains_code_context_and_trace() {
		let mut state = BrowserState::default();
		BrowserRenderer.fold_args(
			&mut state,
			&omp_core::slopjson::parse(
				r#"{"action":"run","name":"docs","url":"https://bun.sh/docs","code":"await tab.waitFor('aria/Sign in')"}"#,
			)
			.expect("browser args decode"),
			true,
		);
		let fault = BrowserFault {
			code:      Str::new_static("browser_timeout"),
			message:   Str::new_static("TimeoutError: selector <missing>"),
			name:      Some(Str::new_static("docs")),
			url:       Some(Str::new_static("https://bun.sh/docs")),
			title:     None,
			browser:   Some(Str::new_static("headless")),
			operation: None,
		};
		let rendered = BrowserRenderer
			.view(&state, Some(&CallOutcome::Faulted(fault)))
			.expect("browser failure renders");
		assert!(rendered.contains("await tab.waitFor"));
		assert!(rendered.contains("<pre max-rows=16 overflow=lines>"));
		assert!(rendered.contains("<callout kind=error>"));
		assert!(rendered.contains("TimeoutError: selector &lt;missing&gt;"));
		assert!(rendered.contains("headless"));
	}

	#[test]
	fn browser_results_use_json_facts_and_artifact_facts() {
		let mut state = BrowserState::default();
		state.code = Some(Str::new_static("return { ok: true };"));
		let payload = BrowserPayload {
			action:    BrowserAction::Run,
			name:      Str::new_static("main"),
			url:       Some(Str::new_static("https://example.test")),
			title:     Some(Str::new_static("Example")),
			display:   Vec::new(),
			result:    Some(serde_json::json!({
				"display_outputs": [{"value": "<shown>"}],
				"return_value": true
			})),
			artifacts: vec![crate::browser::Artifact {
				uri:      Str::new_static(
					"artifact://sha256/0000000000000000000000000000000000000000000000000000000000000000",
				),
				mime:     Str::new_static("image/png"),
				kind:     Str::new_static("screenshot"),
				visible:  true,
				byte_len: 1,
			}],
			browser:   Some(Str::new_static("headless")),
		};
		let rendered = BrowserRenderer
			.view(&state, Some(&CallOutcome::Ok(payload)))
			.expect("browser result renders");
		assert!(rendered.contains("<fact label=display><json"));
		assert!(rendered.contains("<fact label=return><json"));
		assert!(rendered.contains("&lt;shown&gt;"));
		assert!(rendered.contains("<fact label=Artifact>"));
		assert!(!rendered.contains("ctrl+o"));

		let open = BrowserPayload {
			action:    BrowserAction::Open,
			name:      Str::new_static("docs"),
			url:       Some(Str::new_static("https://example.test/docs")),
			title:     Some(Str::new_static("Documentation")),
			display:   Vec::new(),
			result:    Some(serde_json::json!({"ready": true, "status": "<loaded>"})),
			artifacts: Vec::new(),
			browser:   None,
		};
		let rendered = BrowserRenderer
			.view(&BrowserState::default(), Some(&CallOutcome::Ok(open)))
			.expect("browser open renders");
		assert!(rendered.contains(r#"open tab "docs""#));
		assert!(rendered.contains("https://example.test/docs"));
		assert!(rendered.contains("Documentation"));
		assert!(rendered.contains("<json max-depth=3 max-rows=6 max-chars=100>"));
		assert!(rendered.contains("&lt;loaded&gt;"));
	}

	#[test]
	fn computer_structured_result_artifact_and_fault_are_semantic() {
		let payload = ComputerPayload {
			action:       crate::computer::Action::Run,
			code:         Some(Str::new_static("await desktop.screenshot()")),
			results:      vec![serde_json::json!({"width": 1440, "height": 900})],
			artifacts:    vec![crate::computer::Artifact {
				uri:           Str::new_static(
					"artifact://sha256/0000000000000000000000000000000000000000000000000000000000000000",
				),
				mime:          Str::new_static("image/png"),
				visible:       true,
				byte_len:      1,
				width:         1440,
				height:        900,
				source_width:  1440,
				source_height: 900,
				target:        Str::new_static("desktop"),
			}],
			capabilities: None,
		};
		let rendered = ComputerRenderer
			.view(&ComputerState::default(), Some(&CallOutcome::Ok(payload)))
			.expect("computer result renders");
		assert!(rendered.contains("<json max-depth=4 max-rows=10 max-chars=100>"));
		assert!(rendered.contains("<fact label=Screenshot>"));
		assert!(rendered.contains("artifact://sha256/"));

		let fault = ComputerFault {
			code:      crate::computer::FaultCode::PermissionDenied,
			message:   Str::new_static("Screen Recording <required>"),
			operation: Some(crate::computer::Operation::Capture),
		};
		let rendered = ComputerRenderer
			.view(&ComputerState::default(), Some(&CallOutcome::Faulted(fault)))
			.expect("computer fault renders");
		assert!(rendered.contains("<callout kind=error>"));
		assert!(rendered.contains("Screen Recording &lt;required&gt;"));
	}
}
