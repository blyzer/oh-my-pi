use super::{CardFixture, FixtureState};

const ARGS: &str = r#"{"op":"status","provider":"github.com","pipeline":"42","watch":true}"#;
const RESULT: &str = r#"{"output":"github.com 42 failure\nfailed job"}"#;

pub(super) const FIXTURES: &[CardFixture] = &[CardFixture {
	tool:   "ci",
	title:  "CI",
	states: [
		FixtureState {
			args:   r#"{"op":"status","provider":"github.com","pipeline":"4"#,
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
			fault:  Some(r#""ci pipeline 42 was not found on github.com""#),
		},
	],
}];
