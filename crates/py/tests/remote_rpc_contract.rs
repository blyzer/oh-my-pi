//! Frozen Python remote-shipping and public RPC SDK behavior contracts.

use omp_py::{Engine, pyo3::ffi::c_str};

#[test]
fn top_level_lambda_uses_pickle_and_rpc_sdk_is_importable() {
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| {
			py.run(
				c_str!(
					r#"
import importlib.util
import pathlib
import sys
import tempfile

import omp_remote
from omp_rpc import MessageUpdateEvent, RpcClient, parse_notification

with tempfile.TemporaryDirectory() as directory:
    module_path = pathlib.Path(directory) / "shipping_contract.py"
    module_path.write_text("named = lambda value: value + 1\n", encoding="utf-8")
    spec = importlib.util.spec_from_file_location("shipping_contract", module_path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    assert omp_remote._default_ship(module.named) == "pickle"
    assert omp_remote._pack_function(module.named, None)[1]

client = RpcClient(command=("omp", "--mode", "rpc"))
assert client is not None
notification = parse_notification({
    "type": "message_update",
    "message": {"role": "assistant"},
    "assistantMessageEvent": {
        "type": "text_delta",
        "contentIndex": 0,
        "partial": {"role": "assistant"},
        "delta": "hello",
    },
})
assert isinstance(notification, MessageUpdateEvent)
assert notification.assistant_message_event["delta"] == "hello"
"#
				),
				None,
				None,
			)
		})
		.expect("remote shipping and RPC SDK contract");
}

/// `set_custom_tools` sends a host tool's effect envelope only when it declared
/// one: an omitted envelope is undeclared, which the agent admits at the exec
/// tier, while `{}` declares a tool with no effects.
#[test]
fn host_tools_send_effects_only_when_declared() {
	let engine = Engine::builder().init().expect("embedded Python boots");
	engine
		.attach(|py| {
			py.run(
				c_str!(
					r#"
from omp_rpc import RpcClient, host_tool

sent = []
client = RpcClient(command=("omp", "--mode", "rpc"))
client._process = object()
def fake_request(command, **payload):
    sent.append((command, payload))
    return {"toolNames": [tool["name"] for tool in payload["tools"]]}
client._request = fake_request

execute = lambda params, context: "ok"
names = client.set_custom_tools([
    host_tool(name="undeclared", description="d", parameters={"type": "object"}, execute=execute),
    host_tool(name="pure", description="d", parameters={"type": "object"}, execute=execute, effects={}),
    host_tool(
        name="reader",
        description="d",
        parameters={"type": "object"},
        execute=execute,
        effects={"documents": {"read": True}},
    ),
])
assert names == ("undeclared", "pure", "reader")
(command, payload), = sent
assert command == "set_host_tools"
tools = {tool["name"]: tool for tool in payload["tools"]}
assert "effects" not in tools["undeclared"]
assert tools["pure"]["effects"] == {}
assert tools["reader"]["effects"] == {"documents": {"read": True}}
"#
				),
				None,
				None,
			)
		})
		.expect("host tool effects contract");
}
