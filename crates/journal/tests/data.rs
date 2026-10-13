//! Exact JSON shapes for typed journal payloads.

use omp_core::Str;
use omp_journal::{Kind, KindName, data, kind};
use serde_json::{json, value::RawValue};

#[test]
fn closed_kind_vocabulary_round_trips_through_text() {
	let names = [
		kind::JOURNAL,
		kind::TURN_START,
		kind::MSG_USER,
		kind::MSG_ASSISTANT_START,
		kind::STREAM,
		kind::MSG_ASSISTANT_END,
		kind::TOOL_CALL,
		kind::TOOL_UPDATE,
		kind::TOOL_RESULT,
		kind::TURN_RECEIPT,
		kind::PATCH,
		kind::COMPACTION,
	];
	for name in names {
		let parsed: Kind = format!("{name}@1").parse().expect("known kind parses");
		assert!(parsed.is_known());
		assert_eq!(parsed.to_string(), format!("{name}@1"));
		let vocabulary: KindName = name.parse().expect("kind-name parses");
		assert_eq!(vocabulary.to_string(), name);
	}
	assert!(
		!"x.private@1"
			.parse::<Kind>()
			.expect("valid extension grammar")
			.is_known()
	);
	assert!("PATCH".parse::<KindName>().is_err());
	assert!(
		!"PATCH@1"
			.parse::<Kind>()
			.expect("valid kind grammar")
			.is_known()
	);
}

#[test]
fn tool_result_uses_outcome_or_fault_key_without_wrapper_tags() {
	let outcome = data::ToolResult::Outcome {
		outcome:      RawValue::from_string(json!({"text": "ok"}).to_string()).expect("raw JSON"),
		prompt_parts: None,
		source_blob:  None,
	};
	let fault = data::ToolResult::Fault {
		fault:        RawValue::from_string(json!({"code": "denied"}).to_string()).expect("raw JSON"),
		prompt_parts: None,
		source_blob:  None,
	};
	let outcome_json = serde_json::to_string(&outcome).expect("serialize");
	let fault_json = serde_json::to_string(&fault).expect("serialize");
	assert_eq!(outcome_json, r#"{"outcome":{"text":"ok"}}"#);
	assert_eq!(fault_json, r#"{"fault":{"code":"denied"}}"#);
	assert!(matches!(
		serde_json::from_str::<data::ToolResult>(&outcome_json).expect("deserialize outcome"),
		data::ToolResult::Outcome { .. }
	));
	assert!(matches!(
		serde_json::from_str::<data::ToolResult>(&fault_json).expect("deserialize fault"),
		data::ToolResult::Fault { .. }
	));
	assert!(matches!(
		serde_json::from_str::<data::ToolResult>(r#"{"outcome":null}"#)
			.expect("deserialize null outcome"),
		data::ToolResult::Outcome { .. }
	));
	assert!(matches!(
		serde_json::from_str::<data::ToolResult>(r#"{"fault":null}"#)
			.expect("deserialize null fault"),
		data::ToolResult::Fault { .. }
	));
	assert!(serde_json::from_str::<data::ToolResult>(r#"{"outcome":null,"fault":null}"#).is_err());
}

#[test]
fn tool_result_tolerates_unknown_fields_but_stays_exactly_one_of_outcome_or_fault() {
	let outcome = serde_json::from_str::<data::ToolResult>(
		r#"{"outcome":{"text":"ok"},"added_by_newer_writer":{"nested":[1,2]}}"#,
	)
	.expect("an extra field must not reject the payload");
	assert!(matches!(outcome, data::ToolResult::Outcome { .. }));
	let fault = serde_json::from_str::<data::ToolResult>(
		r#"{"added_by_newer_writer":7,"fault":{"code":"denied"},"prompt_parts":[]}"#,
	)
	.expect("an extra field must not reject a fault");
	assert!(matches!(fault, data::ToolResult::Fault { .. }));
	assert!(
		serde_json::from_str::<data::ToolResult>(r#"{"outcome":{},"fault":{},"extra":1}"#).is_err(),
		"both outcome and fault still fails"
	);
	assert!(
		serde_json::from_str::<data::ToolResult>(r#"{"extra":1}"#).is_err(),
		"neither outcome nor fault still fails"
	);
}

#[test]
fn stream_payload_omits_fields_not_used_by_the_operation() {
	let payload = data::Stream {
		sid:  7,
		op:   data::StreamOp::Append,
		node: None,
		prop: None,
		text: Some(Str::new_static("delta")),
	};
	assert_eq!(
		serde_json::to_string(&payload).expect("serialize"),
		r#"{"sid":7,"op":"append","text":"delta"}"#
	);
}
