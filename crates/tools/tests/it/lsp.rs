//! Revision contracts for `lsp@4`: every earlier revision lifts onto the live
//! one through a real registry, and nothing earlier stays addressable.

use std::{future::Future, time::Duration};

use bytes::Bytes;
use omp_core::{Str, sf};
use omp_tool::{
	CallOutcome, Claims, Precedence, Presentation, ProjectedCall, RecordedCallOwned, Registry, Rev,
	ToolIdentity,
};
use omp_tools::lsp::{self, Action, Fault, LspControl, Params, Payload};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
struct NoHost;

impl LspControl for NoHost {
	fn execute(
		&self,
		_: Params,
		_: Duration,
		_: CancellationToken,
	) -> impl Future<Output = Result<Payload, Fault>> + Send + '_ {
		std::future::ready(Err(Fault::Unavailable))
	}
}

fn registry() -> Registry {
	let mut registry = Registry::new();
	registry
		.register(
			lsp::tool(NoHost, Duration::from_secs(300)),
			// `lsp` is not a wire-roster slot: it rides the dynamic device surface.
			Presentation::Device,
			Claims {
				precedence: Precedence::ENHANCEMENT,
				claimant:   sf!("omp/core"),
				replaces:   None,
			},
		)
		.expect("lsp registers");
	registry
}

fn recorded(rev: u16, raw_args: &'static [u8], verdict: &[u8]) -> RecordedCallOwned {
	RecordedCallOwned {
		identity: ToolIdentity { name: sf!("lsp"), rev: Rev { family: Default::default(), n: rev } },
		raw_args: Bytes::from_static(raw_args),
		verdict:  Bytes::copy_from_slice(verdict),
	}
}

fn ok_verdict(action: Action, output: &str, data: serde_json::Value) -> Vec<u8> {
	serde_json::to_vec(&CallOutcome::<Payload, Fault>::Ok(Payload {
		action,
		servers: vec![sf!("rust-analyzer")],
		output: Str::new(output),
		data,
		omitted: 0,
	}))
	.expect("verdict serializes")
}

fn live_output(projected: ProjectedCall) -> (Rev, Bytes, String) {
	let ProjectedCall::Live(lifted) = projected else {
		panic!("the call must lift onto the live revision");
	};
	let CallOutcome::Ok(payload) =
		serde_json::from_slice::<CallOutcome<Payload, Fault>>(&lifted.verdict).expect("verdict")
	else {
		panic!("a successful verdict stays successful");
	};
	(lifted.identity.rev, lifted.raw_args, payload.output.to_string())
}

#[test]
fn registry_lifts_every_earlier_lsp_revision_onto_the_live_one() {
	let live = lsp::spec().rev;
	assert_eq!(live, Rev { family: Default::default(), n: 4 });
	let registry = registry();
	let args: &'static [u8] = br#"{"i":"Listing","action":"symbols","file":"src/lib.rs"}"#;
	let symbols = serde_json::json!([
		{"name": "Widget", "kind": 23, "location": {"uri": "file:///w/src/lib.rs", "range": {"start": {"line": 2, "character": 0}, "end": {"line": 6, "character": 1}}}},
		{"name": "new", "kind": 6, "containerName": "Widget", "location": {"uri": "file:///w/src/lib.rs", "range": {"start": {"line": 9, "character": 1}, "end": {"line": 13, "character": 2}}}}
	]);
	let verdict =
		ok_verdict(Action::Symbols, "struct Widget @ line 3\nmethod new @ line 10\n", symbols);
	for earlier in 1..=3 {
		let (rev, raw_args, output) =
			live_output(registry.project(recorded(earlier, args, &verdict)));
		assert_eq!(rev, live, "lsp@{earlier} is lifted to the live revision, not served as itself");
		assert_eq!(raw_args.as_ref(), args, "lsp@{earlier} arguments are kept byte for byte");
		assert_eq!(
			output, "struct Widget @ lines 3-7\nmethod new in Widget @ lines 10-14\n",
			"lsp@{earlier} document symbols gain ranges and containers"
		);
	}
}

#[test]
fn navigation_verdicts_are_reprojected_and_the_live_revision_passes_through() {
	let registry = registry();
	let args: &'static [u8] =
		br#"{"action":"references","file":"src/lib.rs","line":4,"symbol":"new"}"#;
	let verdict = ok_verdict(Action::References, "stale text", serde_json::json!([]));
	for earlier in 1..=3 {
		let (_, _, output) = live_output(registry.project(recorded(earlier, args, &verdict)));
		assert_eq!(output, "No references found", "lsp@{earlier}");
	}
	let original = recorded(4, args, &verdict);
	let ProjectedCall::Live(same) = registry.project(original.clone()) else {
		panic!("the live revision is live");
	};
	assert_eq!(same.verdict, original.verdict, "the live revision is served untouched");
}

#[test]
fn calls_that_no_longer_decode_stay_as_data() {
	let registry = registry();
	let verdict = ok_verdict(Action::Status, "ok", serde_json::json!([]));
	for rev in [1, 2, 3] {
		assert!(
			matches!(
				registry.project(recorded(rev, br#"{"action":"unknown"}"#, &verdict)),
				ProjectedCall::Data(_)
			),
			"lsp@{rev} with an unknown action is kept verbatim"
		);
	}
	assert!(
		matches!(
			registry.project(recorded(5, br#"{"action":"status"}"#, &verdict)),
			ProjectedCall::Data(_)
		),
		"a revision newer than the live one is not lifted"
	);
}
