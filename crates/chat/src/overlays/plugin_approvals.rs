//! `/plugins approve`: a centered list of every plugin command the session
//! did not start because the operator has not approved it — each row the
//! plugin, the component and its name, and the command line. Enter approves
//! the selected command through the controller's typed mutation stream
//! ([`Mutation::ApprovePluginCommands`]), which records the same approval
//! `omp ext trust --approve-command` does; settled outcomes return through
//! [`Panel::notify`], the list refreshes from the application's feed, and
//! the outcome shows in the selector's own status row until the next key
//! (chat's notice row would paint across the centered box). A footer line
//! names `--plugin-dir`, which loads a plugin from a local directory for one
//! session instead.

use std::sync::Arc;

use omp_core::{Hash32, Str, sf};
use omp_tui::{
	Frame, Key, MouseReport, Prop, Size, Ui, UiContext, UiEvent, components::Select, dom,
};

use super::{
	Outcome, Panel, PanelAnchor, PanelEvent, PanelNote,
	services::{BlockedCommandRow, Mutation, Services},
};
use crate::host::HostCommand;

/// Maximum visible command rows.
const MAX_VISIBLE: usize = 16;
/// Border rows, divider, status row, local-directory hint, and key hint.
const CHROME_ROWS: u16 = 6;
const EMPTY_VALUE: &str = "__empty__";
const COMMANDS_ID: &str = "commands";
const STATUS_ID: &str = "approval-status";
const HINT: &str = "↑/↓ commands · Enter approve · type to search · Esc close";
/// How to load a plugin from a local directory instead of an install; its
/// commands are approved here the same way.
const PLUGIN_DIR_HINT: &str =
	"Load a plugin from a local directory for one session: start omp with --plugin-dir <path>";

/// A settled approval, as the status row shows it.
struct Settled {
	line:   Str,
	failed: bool,
}

/// Retained selector over the session's blocked plugin commands.
pub struct PluginApprovals {
	services:  Arc<dyn Services>,
	rows:      Vec<BlockedCommandRow>,
	in_flight: Option<Mutation>,
	/// The last settled approval's line, shown in the status row until the
	/// next key.
	settled:   Option<Settled>,
	query:     Str,
	ui:        Ui,
	ctx:       UiContext,
	width:     u16,
	visible:   u16,
}

impl PluginApprovals {
	/// Opens the selector over the commands the application reports blocked,
	/// filtered to `query` (a plugin id, from `/plugins approve <plugin>`).
	pub fn open(services: &Arc<dyn Services>, query: Str, ctx: &UiContext) -> Result<Self, Str> {
		let rows = services
			.blocked_plugin_commands()
			.map_err(|error| sf!("{error}"))?;
		let mut panel = Self {
			services: Arc::clone(services),
			rows,
			in_flight: None,
			settled: None,
			query,
			ui: Ui::from_root(dom! { <col/> }, 80, ctx.clone()),
			ctx: ctx.clone(),
			width: 80,
			visible: 0,
		};
		panel.visible = panel.visible_rows(20);
		panel.rebuild();
		Ok(panel)
	}

	/// Commands the selector lists, in list order.
	#[must_use]
	pub fn rows(&self) -> &[BlockedCommandRow] {
		&self.rows
	}

	/// The approval in flight, as `(plugin id, digest)`.
	#[must_use]
	pub fn in_flight(&self) -> Option<(&str, Option<&Hash32>)> {
		match &self.in_flight {
			Some(Mutation::ApprovePluginCommands { plugin, digest }) => {
				Some((plugin.as_str(), digest.as_ref()))
			},
			_ => None,
		}
	}

	fn visible_rows(&self, height: u16) -> u16 {
		let items = self.rows.len().clamp(1, MAX_VISIBLE) as u16;
		items.min(height.saturating_sub(CHROME_ROWS)).max(1)
	}

	fn status_line(&self) -> Str {
		if let Some((plugin, _)) = self.in_flight() {
			return sf!("Approving {plugin}…");
		}
		if let Some(settled) = &self.settled {
			return settled.line.clone();
		}
		match self.rows.len() {
			0 => Str::new_static("Every plugin command is approved"),
			1 => Str::new_static("1 command awaits approval · approved commands load after /restart"),
			count => sf!("{count} commands await approval · approved commands load after /restart"),
		}
	}

	fn rebuild(&mut self) {
		let options: Vec<(Str, Str, Str, Str, bool)> = self
			.rows
			.iter()
			.map(|row| {
				(
					sf!("{}", row.digest),
					sf!("{} {} `{}`", row.plugin, row.kind, row.server),
					row.command.clone(),
					row.unreadable
						.as_ref()
						.map_or_else(Str::default, |path| sf!("unreadable: {path}")),
					row.unreadable.is_some(),
				)
			})
			.collect();
		let empty = options.is_empty();
		let status = self.status_line();
		let status_fg = self.status_fg();
		let seed = self.query.clone();
		let height = self.visible.saturating_add(1);
		let tree = dom! {
			<box border=round title="Plugin commands" pad-x=1>
				<col>
					<select id={COMMANDS_ID} filter={seed} h={height}>
						if empty {
							<option value={EMPTY_VALUE} label="No blocked commands">
								<td><pre>{"No blocked commands"}</pre></td>
								<td truncate grow><pre fg=muted>{"Nothing awaits approval"}</pre></td>
							</option>
						}
						for (value, label, command, unreadable, refused) in options {
							<option value={value} label={label.clone()}>
								<td><pre>{label}</pre></td>
								// A command line is the plugin root, then the
								// executable and its arguments: clip the root so
								// what would run stays readable.
								<td truncate=start grow><pre fg=muted>{command}</pre></td>
								if refused {
									<td align=end><pre fg=err>{unreadable}</pre></td>
								}
							</option>
						}
					</select>
					<hr border=round/>
					<text id={STATUS_ID} fg={status_fg} truncate>{status}</text>
					<text fg=muted truncate>{PLUGIN_DIR_HINT}</text>
					<text fg=muted truncate>{HINT}</text>
				</col>
			</box>
		};
		// A rebuild (a resize, a refreshed list) keeps the cursor on the
		// command it was on, so Enter never approves a different one than
		// the operator selected; an approved command left the list, and the
		// cursor starts over.
		let highlighted = self
			.ui
			.with_component::<Select, _>(COMMANDS_ID, Select::highlighted_value)
			.flatten();
		self.ui = Ui::from_root(tree, self.width, self.ctx.clone());
		if let Some(value) = highlighted {
			self
				.ui
				.with_component_mut::<Select, _>(COMMANDS_ID, |select| select.highlight_value(&value));
		}
	}

	const fn status_fg(&self) -> &'static str {
		match &self.settled {
			_ if self.in_flight.is_some() => "accent",
			Some(Settled { failed: true, .. }) => "err",
			Some(Settled { failed: false, .. }) => "accent",
			None => "muted",
		}
	}

	/// The next key ends a settled outcome's turn in the status row.
	fn clear_settled(&mut self) {
		if self.settled.take().is_some() {
			self.ui.set_text(STATUS_ID, self.status_line());
			self.ui.set_prop(STATUS_ID, Prop::Fg, self.status_fg());
		}
	}

	/// Approves the command whose digest `value` renders.
	fn choose(&mut self, value: &str) -> PanelEvent {
		if value == EMPTY_VALUE {
			return PanelEvent::Consumed;
		}
		if let Some((plugin, _)) = self.in_flight() {
			return PanelEvent::Notice(sf!("Approving {plugin} first"));
		}
		let Some(row) = self.rows.iter().find(|row| {
			value
				.parse::<Hash32>()
				.is_ok_and(|digest| digest == row.digest)
		}) else {
			return PanelEvent::Consumed;
		};
		if let Some(path) = &row.unreadable {
			return PanelEvent::Notice(sf!(
				"{} {} `{}` names the plugin file `{path}`, which cannot be read; it cannot be \
				 approved",
				row.plugin,
				row.kind,
				row.server
			));
		}
		let mutation =
			Mutation::ApprovePluginCommands { plugin: row.plugin.clone(), digest: Some(row.digest) };
		self.in_flight = Some(mutation.clone());
		self.rebuild();
		PanelEvent::Command(HostCommand::Service(mutation))
	}

	fn route(&mut self, event: UiEvent) -> PanelEvent {
		match event {
			UiEvent::Cancel => PanelEvent::Close,
			UiEvent::Changed { id, value } if id.as_str() == COMMANDS_ID => {
				self.choose(value.as_str())
			},
			UiEvent::Filtered { id, query, .. } if id.as_str() == COMMANDS_ID => {
				self.query = query;
				PanelEvent::Consumed
			},
			_ => PanelEvent::Consumed,
		}
	}
}

impl Panel for PluginApprovals {
	fn id(&self) -> &'static str {
		"plugin-approvals"
	}

	fn anchor(&self) -> PanelAnchor {
		PanelAnchor::Center
	}

	fn key(&mut self, key: Key) -> PanelEvent {
		self.clear_settled();
		let event = self.ui.handle_key(key);
		self.route(event)
	}

	fn paste(&mut self, text: &str) -> PanelEvent {
		self.clear_settled();
		let event = self.ui.handle_paste(text);
		self.route(event)
	}

	fn mouse(&mut self, report: MouseReport) -> PanelEvent {
		let event = self
			.ui
			.handle_mouse_with_mods(report.col, report.row, report.kind, report.mods);
		self.route(event)
	}

	fn frame(&mut self, viewport: Size) -> &Frame {
		let visible = self.visible_rows(viewport.height);
		if visible != self.visible {
			self.visible = visible;
			self
				.ui
				.set_prop(COMMANDS_ID, Prop::H, visible.saturating_add(1));
		}
		if viewport.width != self.width {
			self.width = viewport.width;
			self.rebuild();
		}
		self.ui.frame()
	}

	fn notify(&mut self, note: PanelNote<'_>) -> PanelEvent {
		let PanelNote::Outcome(Outcome::Service(outcome)) = note else {
			return PanelEvent::Ignored;
		};
		if self.in_flight.as_ref() != Some(&outcome.mutation) {
			return PanelEvent::Ignored;
		}
		self.in_flight = None;
		if outcome.result.is_ok()
			&& let Ok(rows) = self.services.blocked_plugin_commands()
		{
			self.rows = rows;
		}
		self.settled = Some(match &outcome.result {
			Ok(line) => Settled { line: line.clone(), failed: false },
			Err(error) => Settled { line: sf!("Plugin approval failed: {error}"), failed: true },
		});
		self.rebuild();
		PanelEvent::Consumed
	}
}

#[cfg(test)]
mod tests {
	use parking_lot::Mutex;

	use super::*;
	use crate::overlays::services::{ServiceOutcome, ServiceResult};

	struct Feed {
		rows: Mutex<Vec<BlockedCommandRow>>,
	}

	impl Services for Feed {
		fn blocked_plugin_commands(&self) -> ServiceResult<Vec<BlockedCommandRow>> {
			Ok(self.rows.lock().clone())
		}
	}

	fn row(plugin: &str, server: &str, unreadable: Option<&str>) -> BlockedCommandRow {
		BlockedCommandRow {
			plugin:     Str::new(plugin),
			kind:       Str::new_static("MCP server"),
			server:     Str::new(server),
			command:    sf!("/plugins/{server}/bin/serve --stdio"),
			digest:     Hash32::sum(format!("{plugin}/{server}").as_bytes()),
			unreadable: unreadable.map(Str::new),
		}
	}

	fn open(feed: &Arc<Feed>, query: &str) -> PluginApprovals {
		let services: Arc<dyn Services> = Arc::clone(feed) as Arc<dyn Services>;
		PluginApprovals::open(&services, Str::new(query), &UiContext::default())
			.expect("selector opens")
	}

	fn feed(rows: Vec<BlockedCommandRow>) -> Arc<Feed> {
		Arc::new(Feed { rows: Mutex::new(rows) })
	}

	#[test]
	fn lists_each_blocked_command_with_its_plugin_component_and_command_line() {
		let feed = feed(vec![row("docs@official", "search", None), row("portable", "local", None)]);
		let mut panel = open(&feed, "");
		let text = omp_tui::frame_text(panel.frame(Size { width: 120, height: 20 }));
		assert!(text.contains("Plugin commands"), "title missing:\n{text}");
		assert!(text.contains("docs@official MCP server `search`"), "row missing:\n{text}");
		assert!(text.contains("/plugins/search/bin/serve --stdio"), "command missing:\n{text}");
		assert!(text.contains("portable MCP server `local`"), "row missing:\n{text}");
		assert!(text.contains("2 commands await approval"), "status missing:\n{text}");
		assert!(text.contains("/restart"), "restart hint missing:\n{text}");
		assert!(text.contains("Enter approve"), "hint missing:\n{text}");
		assert!(text.contains("--plugin-dir <path>"), "local-directory hint missing:\n{text}");
		assert_eq!(panel.key(Key::Esc), PanelEvent::Close);
	}

	#[test]
	fn enter_approves_through_the_controller_then_refreshes_from_the_feed() {
		let first = row("docs@official", "search", None);
		let feed = feed(vec![first.clone(), row("portable", "local", None)]);
		let mut panel = open(&feed, "");
		panel.frame(Size { width: 120, height: 20 });
		let mutation = Mutation::ApprovePluginCommands {
			plugin: first.plugin.clone(),
			digest: Some(first.digest),
		};
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Command(HostCommand::Service(mutation.clone()))
		);
		assert_eq!(panel.in_flight(), Some(("docs@official", Some(&first.digest))));
		let text = omp_tui::frame_text(panel.frame(Size { width: 120, height: 20 }));
		assert!(text.contains("Approving docs@official…"), "pending status missing:\n{text}");
		assert!(
			matches!(panel.key(Key::Enter), PanelEvent::Notice(text) if text.contains("first")),
			"a second Enter waits for the request"
		);
		feed.rows.lock().remove(0);
		let outcome = Outcome::Service(ServiceOutcome {
			mutation,
			result: Ok(Str::new_static("Approved docs@official MCP server `search`")),
		});
		// The outcome shows in the selector's own status row, not chat's
		// notice row, which would paint across the centered box.
		assert_eq!(panel.notify(PanelNote::Outcome(&outcome)), PanelEvent::Consumed);
		assert_eq!(panel.in_flight(), None);
		let text = omp_tui::frame_text(panel.frame(Size { width: 120, height: 20 }));
		assert!(
			!text.contains("│ ❯ docs@official") && !text.contains("│   docs@official"),
			"approved row left the list:\n{text}"
		);
		assert!(
			text.contains("Approved docs@official MCP server `search`"),
			"outcome in the status row:\n{text}"
		);
		// The next key hands the status row back to the count.
		panel.key(Key::Down);
		let text = omp_tui::frame_text(panel.frame(Size { width: 120, height: 20 }));
		assert!(!text.contains("Approved docs@official"), "outcome cleared:\n{text}");
		assert!(text.contains("1 command awaits approval"), "status refreshed:\n{text}");
		// Another mutation's outcome is not this panel's.
		let other = Outcome::Service(ServiceOutcome {
			mutation: Mutation::ReloadExtensions,
			result:   Ok(Str::new_static("Plugins reloaded.")),
		});
		assert_eq!(panel.notify(PanelNote::Outcome(&other)), PanelEvent::Ignored);
	}

	/// A resize rebuilds the selector at the new width; the cursor stays on
	/// the command the operator moved it to, so Enter approves that one.
	#[test]
	fn a_rebuild_keeps_the_cursor_on_the_selected_command() {
		let second = row("portable", "local", None);
		let feed = feed(vec![
			row("docs@official", "search", None),
			second.clone(),
			row("tools@market", "db", None),
		]);
		let mut panel = open(&feed, "");
		panel.frame(Size { width: 120, height: 20 });
		panel.key(Key::Down);
		let text = omp_tui::frame_text(panel.frame(Size { width: 72, height: 12 }));
		assert!(text.contains("❯ portable MCP server `local`"), "cursor moved:\n{text}");
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Command(HostCommand::Service(Mutation::ApprovePluginCommands {
				plugin: second.plugin,
				digest: Some(second.digest),
			}))
		);
	}

	/// Plugin roots are long install paths; the executable and arguments at
	/// the end of the command line are what the operator approves, so a
	/// clipped command drops the root, never the tail.
	#[test]
	fn a_long_command_line_keeps_the_executable_and_arguments_visible() {
		let mut long = row("docs@official", "search", None);
		long.command = sf!(
			"/home/operator/.local/share/omp/plugins/cache/plugins/official___docs___1.0.0/bin/serve \
			 --stdio"
		);
		let feed = feed(vec![long]);
		let mut panel = open(&feed, "");
		let text = omp_tui::frame_text(panel.frame(Size { width: 100, height: 20 }));
		assert!(text.contains("bin/serve --stdio"), "command tail clipped:\n{text}");
		assert!(text.contains("…"), "the root is clipped:\n{text}");
	}

	#[test]
	fn an_unreadable_command_is_listed_but_never_sent_for_approval() {
		let feed = feed(vec![row("docs@official", "search", Some("/plugins/search/bin/serve"))]);
		let mut panel = open(&feed, "");
		let text = omp_tui::frame_text(panel.frame(Size { width: 140, height: 20 }));
		assert!(text.contains("unreadable: /plugins/search/bin/serve"), "marker missing:\n{text}");
		assert!(
			matches!(panel.key(Key::Enter), PanelEvent::Notice(text) if text.contains("cannot be approved")),
			"Enter refuses an unreadable command"
		);
		assert_eq!(panel.in_flight(), None);
	}

	#[test]
	fn opening_for_a_plugin_filters_the_list_and_an_empty_list_says_so() {
		let feed = feed(vec![row("docs@official", "search", None), row("portable", "local", None)]);
		let mut panel = open(&feed, "portable");
		let text = omp_tui::frame_text(panel.frame(Size { width: 120, height: 20 }));
		assert!(text.contains("portable MCP server `local`"), "filtered row missing:\n{text}");
		assert!(!text.contains("docs@official MCP"), "other plugin filtered out:\n{text}");

		let feed = self::feed(Vec::new());
		let mut panel = open(&feed, "");
		let text = omp_tui::frame_text(panel.frame(Size { width: 120, height: 20 }));
		assert!(text.contains("No blocked commands"), "empty row missing:\n{text}");
		assert!(text.contains("Every plugin command is approved"), "status missing:\n{text}");
		assert_eq!(panel.key(Key::Enter), PanelEvent::Consumed);
	}
}
