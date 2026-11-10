//! Typed card for forge operations.

use omp_core::Str;
use omp_dom::{Node, PropId};
use omp_tui::{IntoComponent as _, UiContext, dom};
use serde_json::Value;

use super::{Card, CardView, Component, elapsed_badge, typed_fault, typed_input, typed_result};

/// Renders forge operation summaries and results.
pub struct ForgeCard;

impl Card for ForgeCard {
	fn tool(&self) -> &'static str {
		"forge"
	}

	fn render(&self, view: &CardView<'_>, expanded: bool, _ui: &UiContext) -> Component {
		let args = typed_input::<omp_tools::forge::Params>(view).unwrap_or(Value::Null);
		let action = args
			.get("op")
			.and_then(Value::as_str)
			.map_or_else(|| "Forge".to_owned(), |op| format!("Forge {op}"));
		let detail = meta(&args).join(" · ");
		match view.status.as_str() {
			"ok" => {
				let output = result_text(view);
				dom! {
					<box border=round bc=border title_pad=3 pad="0 1">
						<row kind=title gap=1><i:gh fg=accent/><text fg=accent>{action}</text><text fg=muted>{detail}</text></row>
						<pre fg=output>{output}</pre>
					</box>
				}
				.into_component()
			},
			"error" => {
				let fault = failure(view);
				dom! {
					<box border=round bc=err title_pad=3 pad="0 1">
						<row kind=title gap=1><i:error fg=err/><text fg=err>{action}</text><text fg=muted>{detail}</text></row>
						<text fg=err>{fault}</text>
					</box>
				}
				.into_component()
			},
			_ => {
				dom! {
					<box border=round bc=border title_pad=3 pad="0 1">
						<row kind=title gap=1><i:pending fg=output/><text fg=accent>{action}</text><text fg=muted>{detail}</text>
							if let Some(badge) = elapsed_badge(view) { {badge} }
						</row>
						if expanded {
							<pre fg=output>{args.to_string()}</pre>
						}
					</box>
				}
				.into_component()
			},
		}
	}
}

fn meta(args: &Value) -> Vec<String> {
	let mut meta = Vec::new();
	if let Some(number) = args.get("number").and_then(Value::as_u64) {
		meta.push(format!("#{number}"));
	}
	for key in ["repo", "query", "path", "rev", "title"] {
		if let Some(value) = args
			.get(key)
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|value| !value.is_empty())
		{
			meta.push(value.to_owned());
		}
	}
	meta
}

fn result_text(view: &CardView<'_>) -> String {
	typed_result::<omp_tools::forge::Payload>(view)
		.and_then(|value| {
			value
				.get("output")
				.and_then(Value::as_str)
				.map(str::to_owned)
		})
		.unwrap_or_default()
}

fn failure(view: &CardView<'_>) -> Str {
	if let Some(fault) = typed_fault::<omp_tools::forge::Fault>(view) {
		return fault;
	}
	let raw = view.diag.and_then(node_text).unwrap_or_default();
	serde_json::from_str::<String>(raw.as_str())
		.map(Str::new)
		.unwrap_or(raw)
}

fn node_text(node: &Node) -> Option<Str> {
	node.content.clone().or_else(|| {
		node
			.prop(&PropId::Text.into())
			.and_then(|value| value.as_str())
			.map(Str::new)
	})
}
