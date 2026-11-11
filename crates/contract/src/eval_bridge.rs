//! Parent-session bridge vocabulary for eval runs.
//!
//! The environment authenticates the call and drives the Python worker.
//! A session composition implements [`ParentSessionHost`] and holds the
//! [`ParentBindingLease`] that retires that binding.

use std::{collections::BTreeMap, fmt, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use omp_core::{IntoStr, Str, Ulid, sf};
use parking_lot::Mutex;
use serde_json::Value;
use thiserror::Error;

/// Failure returned by the authenticated host operation serving an eval bridge
/// call.
#[derive(Debug, Error)]
pub enum BridgeHostError {
	/// Model-facing failure message produced by the host-side operation.
	// lintx-allow: error-str-payload model-facing bridge message; rendered once here
	#[error("{0}")]
	Message(Str),
	/// Canonical policy denial produced before a child session exists.
	#[error("child session spawn was denied by policy")]
	PolicyDenied(Arc<omp_tool::PolicyDenied>),
	/// The resolved per-tool approval policy denies this nested native call.
	/// Eval-level approval is the unit of prompts, so an explicit `deny`
	/// refuses without prompting.
	#[error(
		"tool `{tool}` is denied by approval policy (tools.approval.{tool}); no action was taken"
	)]
	ApprovalDenied {
		/// Refused native tool.
		tool: Str,
	},
	/// The calling invocation's roster restrictions (`sv_tools`, a mode
	/// Director's bind, a `turn_start` hook, or plan mode's plan-file scope)
	/// refuse this nested call.
	#[error("nested tool call refused: {0}")]
	RosterRestricted(omp_tool::RosterDenial),
}

impl BridgeHostError {
	/// Constructs a model-facing bridge failure with the provided message.
	pub fn message(message: impl IntoStr) -> Self {
		Self::Message(message.into_str())
	}

	/// Constructs a model-facing bridge failure, rendering the cause once at
	/// this model-facing fault boundary.
	pub fn caused(cause: impl fmt::Display) -> Self {
		Self::Message(sf!("{cause}"))
	}

	/// Constructs a model-facing bridge failure for a failed operation,
	/// rendering the cause once at this model-facing fault boundary.
	pub fn because(operation: impl fmt::Display, cause: impl fmt::Display) -> Self {
		Self::Message(sf!("{operation}: {cause}"))
	}
}

/// Correlated, ordered progress destination for one bridge request.
pub trait BridgeProgressSink: Send + Sync {
	/// Emits one bounded progress event before the terminal response.
	fn progress(&self, event: Value) -> Result<(), BridgeHostError>;
}

/// Progress sink used when a caller does not consume intermediate events.
pub struct NoopBridgeProgress;

impl BridgeProgressSink for NoopBridgeProgress {
	fn progress(&self, _event: Value) -> Result<(), BridgeHostError> {
		Ok(())
	}
}

/// Real host-side boundary used for ordinary tools and the privileged eval
/// completion/agent/workpool/budget operations. Implementations receive only
/// calls that passed grant authentication and capability checks.
#[async_trait]
pub trait BridgeHost: Send + Sync {
	/// Dispatches one authenticated, capability-checked eval request into the
	/// host.
	///
	/// Progress is emitted in request order before the returned terminal value.
	async fn call(
		&self,
		name: &str,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError>;
}

/// Optional capabilities owned by the live parent agent session.
///
/// The environment never synthesizes these operations. A session composition
/// that can perform them supplies this authenticated callback; otherwise the
/// corresponding bridge names are omitted from the grant.
#[async_trait]
pub trait ParentSessionHost: Send + Sync {
	/// Freezes the filesystem and managed-environment authority of the current
	/// parent session.
	fn eval_session_config(&self) -> Result<EvalSessionConfig, BridgeHostError>;

	/// Releases process-local child schedulers when this authenticated parent
	/// binding is retired or replaced. Durable observations remain journaled.
	fn release_eval_owner(&self) {}

	/// Whether this parent owns a completion implementation.
	fn completion_available(&self) -> bool {
		true
	}

	/// Runs a parent-session model completion and forwards ordered progress to
	/// the eval caller.
	async fn completion(
		&self,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError>;

	/// Whether this parent owns a child-agent implementation.
	fn agent_available(&self) -> bool {
		true
	}

	/// Runs a parent-session child agent and forwards ordered progress to the
	/// eval caller.
	async fn agent(
		&self,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError>;

	/// Whether this parent owns a live workpool scheduler. Capability
	/// advertisement is withheld until this returns true.
	fn workpool_available(&self) -> bool {
		false
	}

	/// Creates or operates a parent-owned persistent subagent workpool.
	async fn workpool(
		&self,
		args: Value,
		progress: &dyn BridgeProgressSink,
	) -> Result<Value, BridgeHostError> {
		let _ = (args, progress);
		Err(BridgeHostError::message("eval workpool is unavailable for this parent session"))
	}

	/// Whether this parent owns concurrency controls.
	fn concurrency_available(&self) -> bool {
		true
	}

	/// Applies the parent session's concurrency operation to extension-supplied
	/// arguments.
	async fn concurrency(&self, args: Value) -> Result<Value, BridgeHostError>;

	/// Whether this parent owns budget controls.
	fn budget_available(&self) -> bool {
		true
	}

	/// Reads or updates parent-session budget state using extension-supplied
	/// arguments.
	async fn budget(&self, args: Value) -> Result<Value, BridgeHostError>;
}

/// Filesystem authority projected by a live parent session into one eval run.
///
/// Session and artifact access never enters the child environment: `output()`
/// uses the authenticated Read bridge against journal-derived agent/job views
/// and the session CAS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalSessionConfig {
	/// Environment-authorized working directory for each cell.
	pub cwd:              PathBuf,
	/// Serialized bounded `local://` root projection, or removal when absent.
	pub local_roots_json: Option<Str>,
}

struct ParentBinding {
	generation: Ulid,
	parent:     Arc<dyn ParentSessionHost>,
}

/// Revocable table of parent-session bindings keyed by authenticated owner.
#[derive(Clone)]
pub struct ParentRegistry {
	parents: Arc<Mutex<BTreeMap<Str, ParentBinding>>>,
}

impl ParentRegistry {
	/// Creates an empty parent table.
	#[must_use]
	pub fn new() -> Self {
		Self { parents: Arc::new(Mutex::new(BTreeMap::new())) }
	}

	/// Binds `parent` to `owner`.
	///
	/// # Errors
	///
	/// Returns a bridge failure when `owner` is empty or already bound.
	pub fn bind(
		&self,
		owner: Str,
		parent: Arc<dyn ParentSessionHost>,
	) -> Result<ParentBindingLease, BridgeHostError> {
		if owner.is_empty() {
			return Err(BridgeHostError::message("eval bridge parent owner is empty"));
		}
		let generation = Ulid::generate();
		let mut parents = self.parents.lock();
		if parents.contains_key(&owner) {
			return Err(BridgeHostError::message("eval bridge parent owner is already bound"));
		}
		parents.insert(owner.clone(), ParentBinding { generation, parent });
		drop(parents);
		Ok(ParentBindingLease { parents: Arc::clone(&self.parents), owner, generation })
	}

	/// Resolves the parent bound to `owner`, accepting a `(session, agent)`
	/// JSON owner that falls back to the session key.
	///
	/// # Errors
	///
	/// Returns a bridge failure when no binding matches.
	pub fn parent_for(
		&self,
		owner: &str,
	) -> Result<(Str, Ulid, Arc<dyn ParentSessionHost>), BridgeHostError> {
		let parents = self.parents.lock();
		if let Some(binding) = parents.get(owner) {
			return Ok((Str::new(owner), binding.generation, Arc::clone(&binding.parent)));
		}
		let Ok((session, agent)) = serde_json::from_str::<(Str, Str)>(owner) else {
			return Err(BridgeHostError::message(
				"eval bridge parent session is not bound for this owner",
			));
		};
		if session.is_empty() || agent.is_empty() {
			return Err(BridgeHostError::message(
				"eval bridge parent session is not bound for this owner",
			));
		}
		let Some(binding) = parents.get(&session) else {
			return Err(BridgeHostError::message(
				"eval bridge parent session is not bound for this owner",
			));
		};
		Ok((session, binding.generation, Arc::clone(&binding.parent)))
	}

	/// Whether the binding for `owner` is still the generation `generation`.
	#[must_use]
	pub fn generation_matches(&self, owner: &str, generation: Ulid) -> bool {
		self
			.parents
			.lock()
			.get(owner)
			.is_some_and(|binding| binding.generation == generation)
	}

	/// Whether no parent is bound.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.parents.lock().is_empty()
	}

	/// Whether every bound parent satisfies `predicate`.
	#[must_use]
	pub fn all(&self, predicate: impl Fn(&dyn ParentSessionHost) -> bool) -> bool {
		self
			.parents
			.lock()
			.values()
			.all(|binding| predicate(binding.parent.as_ref()))
	}
}

impl Default for ParentRegistry {
	fn default() -> Self {
		Self::new()
	}
}

/// Revocable parent binding for one embedded session owner.
#[must_use]
pub struct ParentBindingLease {
	parents:    Arc<Mutex<BTreeMap<Str, ParentBinding>>>,
	owner:      Str,
	generation: Ulid,
}

impl Drop for ParentBindingLease {
	fn drop(&mut self) {
		let parent = {
			let mut parents = self.parents.lock();
			if parents
				.get(&self.owner)
				.is_some_and(|binding| binding.generation == self.generation)
			{
				parents.remove(&self.owner).map(|binding| binding.parent)
			} else {
				None
			}
		};
		if let Some(parent) = parent {
			parent.release_eval_owner();
		}
	}
}
