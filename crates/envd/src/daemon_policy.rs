//! The sandbox and approval policy a project environment daemon enforces.
//!
//! A daemon compiles its command sandbox, its egress broker and its approval
//! posture from the control context it starts under and keeps them for its
//! whole life. A client that resolved a different policy from its own context
//! must never run its tools there: a client configured with
//! `sv_sandbox_mode off` would have its commands confined and auto-approved by
//! a daemon started under the shipped `workspace-write`, and a client expecting
//! the sandbox would run unconfined under a daemon started without one.
//!
//! [`from_con`] digests everything a daemon fixes at start that decides how a
//! command is confined, wrapped or admitted, and whether the daemon's `read`
//! may reach the network. The digest keys the environment socket
//! (`omp_env::project_state::environment_socket`), so differing clients reach
//! daemons of their own, and the daemon reports it in every `ServerHello`, so a
//! client verifies the daemon it reached before attaching.
//!
//! The rest of what a daemon fixes at start is deliberately left out, so it
//! never splits clients across daemons: edit, read and grep limits, timeouts,
//! direnv, the browser it drives and the memory backend change what a tool
//! does, not how a command is confined or admitted or whether a tool reaches
//! the network by itself. The tool roster is left out as well: an attached
//! session composes its registry, and with it which tools exist, from its own
//! context (`EnvServer::open_session_host`).

use std::collections::BTreeMap;

use omp_con::Ctx;
use omp_core::{Hash32, Str};
use omp_env::project_state::DaemonPolicy;
use serde::Serialize;

use crate::{
	admission::{ApprovalPolicy, ConfiguredApproval},
	exec_settings::{SandboxSettings, ShellSettings},
	tool_settings::ToolSettings,
};

/// Separates the policy digest from every other SHA-256 use; bump it when the
/// digested facts change meaning.
const DOMAIN: &[u8] = b"omp/envd-daemon-policy/v1\0";

/// The facts digested, in a fixed order.
#[derive(Serialize)]
struct PolicyFacts<'a> {
	/// Every `sv_sandbox_*` setting, with who set the network mode and whether
	/// the user set any of them: the posture, the broker and the failure mode
	/// of a sandbox that cannot be constructed.
	sandbox:        &'a SandboxSettings,
	/// `sv_tools_approval_mode` and who set it.
	approval:       ConfiguredApproval,
	/// `sv_tools_approval`: the per-tool overrides honoured in every mode.
	tool_approvals: &'a BTreeMap<Str, ApprovalPolicy>,
	/// `sv_shell_command_prefix`: the wrapper the daemon's shell places before
	/// every admitted command, which a user may make a confinement layer of
	/// its own (`sandbox-exec`, `bwrap`, `timeout`).
	command_prefix: Option<&'a str>,
	/// `sv_fetch_enabled`: whether the daemon's `read` fetches URLs.
	fetch_enabled:  bool,
}

/// Resolves the policy a daemon composed from `ctx` enforces.
///
/// A per-connection approval-mode flag is not part of it: each connection
/// sends its own in `ClientHello.approval_mode`.
#[must_use]
pub fn from_con(ctx: &Ctx) -> DaemonPolicy {
	of(&SandboxSettings::from_con(ctx), &ToolSettings::from_con(ctx), &ShellSettings::from_con(ctx))
}

/// Digests the policy of resolved `sandbox`, `tools` and `shell` settings,
/// before any composition-level approval-mode override.
#[must_use]
pub(crate) fn of(
	sandbox: &SandboxSettings,
	tools: &ToolSettings,
	shell: &ShellSettings,
) -> DaemonPolicy {
	let facts = PolicyFacts {
		sandbox,
		approval: tools.configured_approval(),
		tool_approvals: &tools.approval,
		command_prefix: shell.command_prefix.as_deref(),
		fetch_enabled: tools.fetch_enabled,
	};
	let mut digest = Hash32::hasher();
	digest.update(DOMAIN);
	serde_json::to_writer(&mut digest, &facts)
		.expect("policy facts have string map keys and a hasher sink never fails");
	DaemonPolicy::new(digest.finalize())
}

#[cfg(test)]
mod tests {
	use omp_con::{Kv, Value};

	use super::*;
	use crate::{
		exec_settings::{
			ExecSandboxMode, SV_SANDBOX_MODE, SV_SANDBOX_NETWORK_MODE, SandboxNetworkMode,
		},
		tool_settings::{ApprovalMode, SV_TOOLS_APPROVAL, SV_TOOLS_APPROVAL_MODE},
	};

	fn policy(configure: impl FnOnce(&Ctx)) -> DaemonPolicy {
		let ctx = Ctx::new();
		configure(&ctx);
		from_con(&ctx)
	}

	/// Equal configurations resolve one policy, and every setting a daemon
	/// fixes at start, or who set it, resolves another.
	#[test]
	fn every_sandbox_and_approval_fact_changes_the_policy() {
		let shipped = policy(|_| {});
		assert_eq!(shipped, policy(|_| {}), "the digest is deterministic");

		let variants = [
			policy(|ctx| {
				SV_SANDBOX_MODE
					.set(ctx, ExecSandboxMode::Off)
					.expect("mode");
			}),
			policy(|ctx| {
				SV_SANDBOX_MODE
					.set(ctx, ExecSandboxMode::WorkspaceWrite)
					.expect("explicit shipped mode");
			}),
			policy(|ctx| {
				SV_SANDBOX_NETWORK_MODE
					.set(ctx, SandboxNetworkMode::Open)
					.expect("network");
			}),
			policy(|ctx| {
				ctx.run("sv_sandbox_allow_localhost 1").expect("loopback");
			}),
			policy(|ctx| {
				SV_TOOLS_APPROVAL_MODE
					.set(ctx, ApprovalMode::AlwaysAsk)
					.expect("approval mode");
			}),
			policy(|ctx| {
				SV_TOOLS_APPROVAL_MODE
					.set(ctx, ApprovalMode::Yolo)
					.expect("explicit shipped approval mode");
			}),
			policy(|ctx| {
				SV_TOOLS_APPROVAL
					.set(ctx, Kv(vec![(Str::new_static("bash"), Value::Str(Str::new_static("deny")))]))
					.expect("per-tool override");
			}),
			policy(|ctx| {
				ctx.run("sv_shell_command_prefix timeout")
					.expect("command prefix");
			}),
			policy(|ctx| {
				ctx.run("sv_fetch_enabled false").expect("fetch policy");
			}),
		];
		for (index, variant) in variants.iter().enumerate() {
			assert_ne!(*variant, shipped, "variant {index} kept the shipped policy");
			for other in &variants[index + 1..] {
				assert_ne!(variant, other, "two variants share a policy");
			}
		}
	}

	/// Settings outside the sandbox and approval policy leave the daemon key
	/// alone, so they never split clients across daemons.
	#[test]
	fn unrelated_settings_keep_the_policy() {
		let shipped = policy(|_| {});
		assert_eq!(
			shipped,
			policy(|ctx| {
				crate::tool_settings::SV_TOOLS_READ_LINE_NUMBERS
					.set(ctx, true)
					.expect("read setting");
			})
		);
	}
}
