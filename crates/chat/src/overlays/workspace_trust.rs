//! First-run workspace trust selection, before an agent session is composed.

use omp_core::{Str, sf};
use omp_tui::{
	AppEvent, AppOptions, Dim, Key, OverlayAnchor, OverlayMargin, OverlayOptions, Size, Ui, dom,
};

/// The operator's answer; persistence belongs to the launching application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TrustChoice {
	/// Continue without loading gated workspace inputs.
	#[default]
	Deny,
	/// Admit inputs only for this process's session.
	Session,
	/// Persist an exact grant bound to the displayed inventory.
	Workspace,
	/// Pin this workspace under the displayed existing subtree grant.
	Pin,
}

/// Presentation-only first-run prompt. Paths and inventory are supplied by the
/// host.
pub struct TrustPrompt {
	/// Canonical workspace displayed to the operator.
	pub workspace: Str,
	/// Gated input paths, relative to the workspace.
	pub inputs:    Vec<Str>,
	/// Whether a previous inventory digest no longer matches.
	pub changed:   bool,
	/// Existing subtree that can admit a pin, never a new broad grant.
	pub subtree:   Option<Str>,
}

#[derive(Default)]
struct Selection {
	index: usize,
}

impl Selection {
	fn key(&mut self, key: Key, pin: bool) -> Option<TrustChoice> {
		let choices =
			[TrustChoice::Deny, TrustChoice::Session, TrustChoice::Workspace, TrustChoice::Pin];
		let count = if pin { 4 } else { 3 };
		match key {
			Key::Esc | Key::Ctrl('c') => Some(TrustChoice::Deny),
			Key::Enter => Some(choices[self.index]),
			Key::Up => {
				self.index = (self.index + count - 1) % count;
				None
			},
			Key::Down | Key::Tab => {
				self.index = (self.index + 1) % count;
				None
			},
			_ => None,
		}
	}
}

impl TrustPrompt {
	fn show(&self, ui: &mut Ui, selection: &Selection, viewport: Size) {
		let _ = ui.close_top_overlay();
		let mut labels = smallvec::SmallVec::<Str, 4>::from([
			Str::new_static("Do not trust"),
			Str::new_static("Trust this session"),
			Str::new_static("Trust this workspace"),
		]);
		if let Some(root) = &self.subtree {
			labels.push(sf!("Trust under {root}"));
		}
		let rows = viewport.height.saturating_sub(16).clamp(1, 12);
		let tree = dom! {
			<box border=round title="Workspace trust" pad-x=1>
				<col>
					<text wrap>{self.workspace.clone()}</text>
					if self.changed { <text fg=warn wrap>{"Workspace inputs changed since your previous approval. Review and trust again."}</text> }
					<text wrap>{"These project inputs can execute code or change agent instructions:"}</text>
					<scroll id="trust-inputs" h={rows} focus>
						<col>
							for input in &self.inputs { <text fg=muted wrap>{input.clone()}</text> }
						</col>
					</scroll>
					for (index, label) in labels.into_iter().enumerate() {
						<row>
							if index == selection.index { <icon name="cursor" fg=accent/> } else { <pre>{"  "}</pre> }
							<text fg={if index == selection.index { "accent" } else { "fg" }} wrap>{label}</text>
						</row>
					}
					<text fg=muted wrap>{"Up/Down select | PgUp/PgDn inputs | Enter confirm | Esc/Ctrl-C deny"}</text>
				</col>
			</box>
		};
		ui.show_overlay(
			tree,
			OverlayOptions::default()
				.anchor(OverlayAnchor::Center)
				.width(Dim::Pct(86))
				.max_height(Dim::Pct(94))
				.margin(OverlayMargin::uniform(1))
				.min_viewport(Size::new(20, 8)),
		);
		ui.focus_first();
	}

	/// Runs the modal on the real terminal and restores it before returning.
	/// Closing the terminal or cancelling always denies trust.
	///
	/// # Errors
	/// Returns terminal I/O failures without granting trust.
	#[expect(
		clippy::future_not_send,
		reason = "the startup selector owns a thread-confined TUI App"
	)]
	pub async fn run(&self) -> std::io::Result<TrustChoice> {
		let mut selection = Selection::default();
		let mut app = AppOptions::new()
			.hold_alt()
			.keep_on_cancel()
			.hotkeys([Key::Up, Key::Down, Key::Tab, Key::Enter, Key::Esc])
			.start(|env: omp_tui::AppEnv| Ui::from_root(dom! { <col/> }, env.viewport.width, env.ctx))
			.await?;
		let mut viewport = app.viewport();
		self.show(app.ui_mut(), &selection, viewport);
		loop {
			match app.next().await? {
				Some(AppEvent::Key(key)) => {
					if let Some(choice) = selection.key(key, self.subtree.is_some()) {
						return Ok(choice);
					}
					self.show(app.ui_mut(), &selection, viewport);
				},
				None | Some(AppEvent::OverlayClosed(_)) => return Ok(TrustChoice::Deny),
				Some(AppEvent::Resized(size)) => {
					viewport = size;
					self.show(app.ui_mut(), &selection, viewport);
				},
				_ => {},
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn enter_defaults_to_do_not_trust() {
		assert_eq!(Selection::default().key(Key::Enter, false), Some(TrustChoice::Deny));
	}

	#[test]
	fn each_explicit_choice_is_reachable() {
		for (steps, choice) in
			[TrustChoice::Deny, TrustChoice::Session, TrustChoice::Workspace, TrustChoice::Pin]
				.into_iter()
				.enumerate()
		{
			let mut state = Selection::default();
			for _ in 0..steps {
				assert_eq!(state.key(Key::Down, true), None);
			}
			assert_eq!(state.key(Key::Enter, true), Some(choice));
		}
	}

	#[test]
	fn cancel_denies_even_after_selecting_trust() {
		for key in [Key::Esc, Key::Ctrl('c')] {
			let mut state = Selection::default();
			state.key(Key::Down, true);
			assert_eq!(state.key(key, true), Some(TrustChoice::Deny));
		}
	}

	#[test]
	fn pin_is_unreachable_without_a_subtree() {
		let mut state = Selection::default();
		state.key(Key::Up, false);
		assert_eq!(state.key(Key::Enter, false), Some(TrustChoice::Workspace));
		state.key(Key::Down, false);
		assert_eq!(state.key(Key::Enter, false), Some(TrustChoice::Deny));
	}

	/// Every option row must paint: an invalid theme token panics at render,
	/// which the key-handling tests above never reach.
	#[test]
	fn every_selection_renders_all_choices() {
		let prompt = TrustPrompt {
			workspace: Str::new_static("/work/team/project"),
			inputs:    vec![Str::new_static(".omp/SYSTEM.md")],
			changed:   true,
			subtree:   Some(Str::new_static("/work/team")),
		};
		let viewport = Size::new(100, 40);
		for index in 0..4 {
			let mut ui = Ui::from_root(dom! { <col/> }, viewport.width, omp_tui::UiContext::default());
			prompt.show(&mut ui, &Selection { index }, viewport);
			let layer = ui
				.top_overlay()
				.and_then(|id| ui.overlay(id))
				.expect("trust overlay shown");
			let text = omp_tui::frame_text(layer.frame());
			for label in ["Do not trust", "Trust this session", "Trust this workspace", "Trust under"]
			{
				assert!(text.contains(label), "{label} missing at selection {index}:\n{text}");
			}
			assert!(text.contains("inputs changed"));
		}
	}
}
