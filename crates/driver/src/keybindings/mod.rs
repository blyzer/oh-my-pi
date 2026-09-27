//! Physical key chords lowered to command-stream bindings.
//!
//! ADR 0014: keybindings are `bind` lines. [`DEFAULT_BINDS`] is the built-in
//! default binding set, executed before the user's `config.cfg`;
//! [`PI_ACTIONS`] maps legacy action ids to the console command each default
//! binds, and [`strip_command`] removes one such command from the bound
//! scripts before a migrated remap installs its own chords. Chord spelling is
//! [`omp_con::normalize_chord`]'s. The app's `omp config migrate` and the v1
//! importer ([`crate::v1_import`]) share these tables.

use omp_con::{ConResult, Ctx};
use omp_core::Str;

/// Default `bind` script, executed before `config.cfg`.
#[cfg(target_os = "macos")]
pub const DEFAULT_BINDS: &str =
	concat!(include_str!("default.cfg"), include_str!("default-macos.cfg"));
/// Default `bind` script, executed before `config.cfg`.
#[cfg(target_os = "windows")]
pub const DEFAULT_BINDS: &str =
	concat!(include_str!("default.cfg"), include_str!("default-windows.cfg"));
/// Default `bind` script, executed before `config.cfg`.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const DEFAULT_BINDS: &str = include_str!("default.cfg");

/// Name of the default bind cfg as reported in command-stream provenance.
pub const DEFAULT_BINDS_NAME: &str = "default-binds.cfg";

/// Legacy keybinding action id → console command used by migration.
///
/// The default cfg may bind one physical chord to a short fallback script
/// for several contextual actions. Each action still has its own command here,
/// so a migrated user remap retains its exact scope.
pub const PI_ACTIONS: &[(&str, &str)] = &[
	("app.interrupt", "cl_interrupt"),
	("app.clear", "cl_clear"),
	("app.exit", "cl_exit"),
	("app.suspend", "cl_suspend"),
	("app.display.reset", "cl_display_reset"),
	("app.thinking.cycle", "cl_thinking_cycle"),
	("app.thinking.toggle", "toggle cl_showthinking"),
	("app.model.cycleForward", "cl_model_cycle"),
	("app.model.cycleBackward", "cl_model_cycle back"),
	("app.model.select", "cl_model_select"),
	("app.model.selectTemporary", "cl_model_select session"),
	("app.tools.expand", "cl_tools_expand"),
	("app.tools.toggleVisibility", "toggle cl_showtools"),
	("app.editor.external", "cl_editor_external"),
	("app.message.followUp", "cl_followup"),
	("app.retry", "cl_retry"),
	("app.plan.toggle", "cl_plan_toggle"),
	("app.history.search", "cl_history_search"),
	("app.message.dequeue", "cl_dequeue"),
	("app.clipboard.pasteImage", "cl_paste_image"),
	("app.clipboard.pasteTextRaw", "cl_paste_raw"),
	("app.clipboard.copyLine", "cl_copy_line"),
	("app.clipboard.copyPrompt", "cl_copy_prompt"),
	("app.agents.hub", "agents"),
	("app.session.observe", "hub"),
	("app.session.togglePath", "panel_toggle_path"),
	("app.session.toggleSort", "panel_toggle_sort"),
	("app.session.rename", "panel_rename"),
	("app.session.delete", "panel_delete"),
	("app.session.deleteNoninvasive", "panel_delete_fast"),
	("app.tree.foldOrUp", "panel_fold_up"),
	("app.tree.unfoldOrDown", "panel_unfold_down"),
	("app.session.new", "new"),
	("app.session.tree", "tree"),
	("app.session.fork", "fork"),
	("app.session.resume", "resume"),
	("app.stt.toggle", "cl_stt_toggle"),
	("app.live.toggle", "live"),
	("tui.editor.cursorUp", "ed_up"),
	("tui.editor.cursorDown", "ed_down"),
	("tui.editor.cursorLeft", "ed_left"),
	("tui.editor.cursorRight", "ed_right"),
	("tui.editor.cursorWordLeft", "ed_word_left"),
	("tui.editor.cursorWordRight", "ed_word_right"),
	("tui.editor.cursorLineStart", "ed_home"),
	("tui.editor.cursorLineEnd", "ed_end"),
	("tui.editor.jumpForward", "ed_jump_forward"),
	("tui.editor.jumpBackward", "ed_jump_backward"),
	("tui.editor.pageUp", "ed_page_up"),
	("tui.editor.pageDown", "ed_page_down"),
	("tui.editor.deleteCharBackward", "ed_backspace"),
	("tui.editor.deleteCharForward", "ed_delete"),
	("tui.editor.deleteWordBackward", "ed_delete_word_backward"),
	("tui.editor.deleteWordForward", "ed_delete_word_forward"),
	("tui.editor.deleteToLineStart", "ed_delete_to_start"),
	("tui.editor.deleteToLineEnd", "ed_delete_to_end"),
	("tui.editor.yank", "ed_yank"),
	("tui.editor.yankPop", "ed_yank_pop"),
	("tui.editor.undo", "ed_undo"),
	("tui.editor.spellingSuggestions", "ed_spelling"),
	("tui.input.newLine", "ed_newline"),
	("tui.input.submit", "ed_enter"),
	("tui.input.tab", "ed_tab"),
	("tui.input.copy", "ed_copy"),
	("tui.select.up", "ed_up"),
	("tui.select.down", "ed_down"),
	("tui.select.pageUp", "ed_page_up"),
	("tui.select.pageDown", "ed_page_down"),
	("tui.select.confirm", "ed_enter"),
	("tui.select.cancel", "cl_interrupt"),
];

/// Console command for a legacy keybinding action id, when supported.
#[must_use]
pub fn pi_action_command(action: &str) -> Option<&'static str> {
	PI_ACTIONS
		.iter()
		.find_map(|(id, command)| (*id == action).then_some(*command))
}

/// Removes `command` from every bound script except on chords `keep`
/// protects, unbinding a chord whose script becomes empty. Other contextual
/// commands sharing a chord keep their order.
///
/// # Errors
///
/// Returns the [`omp_con::ConError`] of rebinding a trimmed script.
pub fn strip_command(ctx: &Ctx, command: &str, keep: impl Fn(&str) -> bool) -> ConResult<()> {
	for (chord, script) in ctx.binds() {
		if keep(chord.as_str()) {
			continue;
		}
		let statements = script.as_str().split(';').map(str::trim);
		if !statements.clone().any(|statement| statement == command) {
			continue;
		}
		let mut kept = String::with_capacity(script.len());
		for statement in statements.filter(|statement| *statement != command) {
			if !kept.is_empty() {
				kept.push_str("; ");
			}
			kept.push_str(statement);
		}
		if kept.is_empty() {
			ctx.unbind(chord.as_str());
		} else {
			ctx.bind(chord, Str::new(kept))?;
		}
	}
	Ok(())
}
