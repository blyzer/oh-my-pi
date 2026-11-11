use super::{CardFixture, FixtureState};

const ARGS: &str = r#"{"op":"search_change_requests","query":"is:open review-requested:@me sort:updated","repo":"oh-my-pi/pi"}"#;
const RESULT: &str = r##"{"output":"#1842 feat(tui): virtualized scrollback\n#1839 fix(agent): retry stream on transient 529\n\n2 results"}"##;

pub(super) const FIXTURES: &[CardFixture] = &[CardFixture {
	tool:   "forge",
	title:  "Forge",
	states: [
		FixtureState {
			args:   r#"{"op":"search_change_requests","query":"is:open author:@me"}"#,
			update: None,
			result: None,
			fault:  None,
		},
		FixtureState { args: ARGS, update: None, result: None, fault: None },
		FixtureState { args: ARGS, update: None, result: Some(RESULT), fault: None },
		FixtureState {
			args:   ARGS,
			update: None,
			result: None,
			fault:  Some(r#""forge repository pi was not found on github.com""#),
		},
	],
}];
