//! The extension host decodes every hook payload into the event's typed
//! dataclass before any hook runs. A value outside an enum's vocabulary makes
//! the whole event fail to decode, and an observe hook's failure is dropped, so
//! the hook simply never runs. `turn_start` carried such a value (`thinking:
//! "none"`, which `omp.Effort` does not have) until the agent loop was fixed.
//!
//! This feeds the payloads each Rust emitter produces, shaped as the emitter
//! builds them, through the host's own decoder (`omp.hooks._value_from_wire`
//! over `omp.events.spec(event).payload`), for the three events that carry a
//! reasoning effort or an intent about it:
//!
//! - `turn_start`: `crates/agent/src/loop.rs`, `thinking` from
//!   `reasoning_effort`;
//! - `model_changed`: `crates/agent/src/model_watch.rs`, `previous_thinking`
//!   and `thinking` as `Option<ReasoningEffort>`; and
//! - `before_request`: `crates/envd/src/tools/hooks.rs`, whose `intents` carry
//!   the `reasoning` intent the request's effort turns into.
//!
//! The list of effort names below must equal the serde names of
//! `omp_catalog::ReasoningEffort`; the test `reasoning_effort_wire_names` in
//! `crates/agent/src/loop.rs` pins the Rust side of the same list.

use std::ffi::CString;

use omp_py::Engine;

const SCRIPT: &str = r#"
import omp
from omp.events import spec as event_spec
from omp.hooks import _value_from_wire

# The serde names of omp_catalog::ReasoningEffort, in declaration order.
RUST_EFFORTS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
assert [effort.value for effort in omp.Effort] == RUST_EFFORTS, list(omp.Effort)


def decode(event, payload):
    return _value_from_wire(event_spec(event).payload, payload)


MODEL = {"provider": "acme", "api": "", "model": "m"}

# turn_start: every effort the loop can report decodes to the same effort.
for name in RUST_EFFORTS:
    turn = decode("turn_start", {
        "turn_id": "t1", "turn_index": 0, "prompt_hash": "p", "toolset_hash": "t",
        "enabled_tools": [], "input_mode": "full", "model": MODEL,
        "route": {"provider": "acme", "route": "acme/m"}, "thinking": name,
        "deadline": None, "attempt": 0, "prompt_changed": True,
        "toolset_changed": False,
    })
    assert turn.thinking is omp.Effort(name), (name, turn.thinking)

# The value the loop used to report for a request that names no effort is not
# part of the vocabulary: the whole event is rejected.
try:
    decode("turn_start", {
        "turn_id": "t1", "turn_index": 0, "prompt_hash": "p", "toolset_hash": "t",
        "enabled_tools": [], "input_mode": "full", "model": MODEL,
        "route": {"provider": "acme", "route": "acme/m"}, "thinking": "none",
        "deadline": None, "attempt": 0, "prompt_changed": True,
        "toolset_changed": False,
    })
except ValueError as error:
    assert "Effort" in str(error), error
else:
    raise AssertionError("an effort outside the vocabulary was accepted")

# model_changed: the Option<ReasoningEffort> fields are an effort or null, and
# the reasons the watch emits are in the vocabulary.
for reason in ("user", "fallback", "role"):
    for previous in [None, *RUST_EFFORTS]:
        for current in [None, *RUST_EFFORTS]:
            changed = decode("model_changed", {
                "from_model": MODEL, "to_model": {**MODEL, "model": "n"},
                "role": "default", "reason": reason,
                "previous_thinking": previous, "thinking": current,
            })
            assert (changed.previous_thinking is None) == (previous is None), changed
            assert (changed.thinking is None) == (current is None), changed
            assert changed.reason.value == reason, changed

# before_request: the draft the provider call builds, with every intent kind it
# can push (`reasoning` is the one a requested effort turns into) and the
# `unspecified` fallback it always writes.
KINDS = [
    "force_call", "strict", "reasoning", "verbosity", "cache_retention",
    "service_tier", "hosted_tool", "safety", "determinism",
]
draft = decode("before_request", {
    "provider": "acme", "route": "acme", "model": "m", "operation": "chat",
    "scalars": {"temperature": 0.5, "top_p": 0.9, "seed": 7, "max_output_tokens": 128},
    "headers": {"x-trace": "1"},
    "intents": [
        {"kind": kind, "on_unsupported": "unspecified", "priority": 0, "payload": None}
        for kind in KINDS
    ],
    "message_count": 3, "approx_prompt_tokens": None,
})
assert [intent.kind.value for intent in draft.intents] == KINDS, draft.intents
assert draft.operation is omp.Operation.CHAT, draft.operation
"#;

#[test]
fn every_payload_a_rust_emitter_builds_decodes_in_the_extension_host() {
	let script = CString::new(SCRIPT).expect("script has no NUL byte");
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| py.run(script.as_c_str(), None, None))
		.unwrap_or_else(|error| {
			panic!("a payload a Rust emitter builds does not decode in the host: {error}")
		});
}
