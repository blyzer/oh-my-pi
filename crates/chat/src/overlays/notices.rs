//! The notice row's log: every status notice chat posts, the one the row
//! shows, and how many arrived beneath it.
//!
//! The row above the composer shows one notice at a time and the next key
//! clears it. A burst (several launch warnings before the first paint, a
//! command reply landing on a warning) used to leave only its last notice
//! visible and the rest in the tracing log. [`NoticeLog`] keeps a bounded
//! history so the row can say `+N more (/notices)` and `/notices` can list
//! them all in a [`ReportPanel`](super::report::ReportPanel); nothing here
//! adds a second surface to the chat layout.

use std::collections::VecDeque;

use omp_core::{Str, sf};

/// Most notices retained; the oldest fall off first.
pub const CAPACITY: usize = 64;

/// The slash command that lists every retained notice.
pub const VIEW_COMMAND: &str = "/notices";

/// Bounded notice history plus the row's visible state.
#[derive(Debug, Default)]
pub struct NoticeLog {
	/// Retained notices, oldest first; consecutive repeats collapse.
	recent:  VecDeque<Str>,
	/// The notice the row shows, until the next key clears it.
	visible: Option<Str>,
	/// The row's `+N more (/notices)` label, formatted when it changes and
	/// never per paint; `None` while nothing is stacked under the visible
	/// notice.
	more:    Option<Str>,
	/// Notices posted since the row was last empty, besides the visible one.
	stacked: usize,
}

impl NoticeLog {
	/// Posts `text`: it becomes the visible notice, joins the history, and
	/// counts as one more under the row when a notice was already showing.
	/// A repeat of the newest notice only refreshes the row.
	pub fn post(&mut self, text: Str) {
		if self.recent.back() != Some(&text) {
			if self.recent.len() == CAPACITY {
				self.recent.pop_front();
			}
			self.recent.push_back(text.clone());
			if self.visible.is_some() {
				self.stacked += 1;
				self.more = Some(sf!("+{} more ({VIEW_COMMAND})", self.stacked));
			}
		}
		self.visible = Some(text);
	}

	/// Empties the row. The history stays for `/notices`.
	pub fn clear_visible(&mut self) {
		self.visible = None;
		self.more = None;
		self.stacked = 0;
	}

	/// The notice the row shows.
	#[must_use]
	pub const fn visible(&self) -> Option<&Str> {
		self.visible.as_ref()
	}

	/// The `+N more (/notices)` label, when notices stacked under the
	/// visible one.
	#[must_use]
	pub const fn more(&self) -> Option<&Str> {
		self.more.as_ref()
	}

	/// How many notices stacked under the visible one.
	#[must_use]
	pub const fn stacked(&self) -> usize {
		self.stacked
	}

	/// Retained notices, oldest first.
	pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Str> + ExactSizeIterator + '_ {
		self.recent.iter()
	}

	/// The `/notices` report body: one markdown bullet per retained notice,
	/// oldest first so a launch burst reads in the order it was posted.
	#[must_use]
	pub fn report(&self) -> Str {
		if self.recent.is_empty() {
			return Str::new_static("No notices yet.");
		}
		let mut body = String::new();
		for notice in &self.recent {
			body.push_str("- ");
			escape_markdown(notice.as_str(), &mut body);
			body.push('\n');
		}
		Str::new(body)
	}
}

/// Appends `text` to `out` so markdown shows it as written: inline code
/// spans keep their contents (commands and flags render as code), every
/// other markdown control character is backslash-escaped, and a notice with
/// unbalanced backticks is escaped whole.
fn escape_markdown(text: &str, out: &mut String) {
	let balanced = text.matches('`').count().is_multiple_of(2);
	let mut in_code = false;
	for ch in text.chars() {
		match ch {
			'`' if balanced => {
				in_code = !in_code;
				out.push(ch);
			},
			'\n' => out.push(' '),
			'\\' | '`' | '*' | '_' | '<' | '>' | '[' | ']' | '#' | '~' | '|' if !in_code => {
				out.push('\\');
				out.push(ch);
			},
			_ => out.push(ch),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn posted(log: &mut NoticeLog, text: &'static str) {
		log.post(Str::new_static(text));
	}

	#[test]
	fn a_burst_shows_its_last_notice_and_counts_the_rest() {
		let mut log = NoticeLog::default();
		posted(&mut log, "first");
		assert_eq!(log.more(), None, "one notice hides nothing");
		posted(&mut log, "second");
		posted(&mut log, "third");
		assert_eq!(log.visible().map(Str::as_str), Some("third"));
		assert_eq!(log.stacked(), 2);
		assert_eq!(log.more().map(Str::as_str), Some("+2 more (/notices)"));
		assert_eq!(log.entries().map(Str::as_str).collect::<Vec<_>>(), ["first", "second", "third"]);
	}

	#[test]
	fn clearing_the_row_keeps_the_history_and_restarts_the_count() {
		let mut log = NoticeLog::default();
		posted(&mut log, "one");
		posted(&mut log, "two");
		log.clear_visible();
		assert_eq!((log.visible(), log.more(), log.stacked()), (None, None, 0));
		posted(&mut log, "three");
		assert_eq!(log.more(), None, "the count restarts after the row emptied");
		assert_eq!(log.entries().len(), 3, "history survives the clear");
	}

	#[test]
	fn a_repeat_of_the_newest_notice_neither_stacks_nor_grows_the_history() {
		let mut log = NoticeLog::default();
		posted(&mut log, "same");
		log.clear_visible();
		posted(&mut log, "same");
		posted(&mut log, "same");
		assert_eq!(log.visible().map(Str::as_str), Some("same"));
		assert_eq!((log.stacked(), log.entries().len()), (0, 1));
	}

	#[test]
	fn history_is_bounded_and_drops_the_oldest() {
		let mut log = NoticeLog::default();
		for index in 0..CAPACITY + 5 {
			log.post(sf!("notice {index}"));
		}
		assert_eq!(log.entries().len(), CAPACITY);
		assert_eq!(log.entries().next().map(Str::as_str), Some("notice 5"));
		assert_eq!(log.visible().map(Str::as_str), Some("notice 68"));
		assert_eq!(log.stacked(), CAPACITY + 4, "the row counts every post, not the retained ones");
	}

	#[test]
	fn report_lists_oldest_first_and_keeps_code_spans_literal() {
		let mut log = NoticeLog::default();
		assert_eq!(log.report().as_str(), "No notices yet.");
		posted(&mut log, "plugin_a: run `omp ext trust <name>` *now*");
		posted(&mut log, "tick `unbalanced");
		assert_eq!(
			log.report().as_str(),
			"- plugin\\_a: run `omp ext trust <name>` \\*now\\*\n- tick \\`unbalanced\n"
		);
	}
}
