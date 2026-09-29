//! The kernel's one `model_changed` emitter.
//!
//! [`ModelWatch`] follows the model the session's next request targets and
//! its effective thinking effort, and raises `model_changed` the moment
//! either changes: a control-plane write (`ai_model`, `ai_thinking`, a role
//! remap through `ai_model_roles`, `ai_external_thinking`) is observed as it
//! commits, whether a user, a client, a Director bind, or a session switch
//! restoring journaled values made it; a request the recovery middleware
//! served on another model (a fallback, or its revert) is observed when the
//! answer starts. Nothing compares requests of one run: an idle session
//! reports a `/model` switch before the next prompt exists.

use std::sync::{Arc, Weak};

use omp_catalog::ReasoningEffort;
use omp_core::Str;
use omp_proto::toolhost::v1::HookEventId;
use parking_lot::Mutex;
use serde::Serialize;
use strum::{Display, EnumString, IntoStaticStr};

use crate::LifecycleHooks;

/// The model the next request targets and the thinking effort it carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSelection {
	/// Catalog model key (`provider/model`).
	pub model:    Str,
	/// The role a role selector (`@plan`) resolved through; `None` when the
	/// selector names a model.
	pub role:     Option<Str>,
	/// The effort the request carries after the model's thinking policy
	/// clamped `ai_thinking`; `None` when it carries no reasoning request.
	pub thinking: Option<ReasoningEffort>,
}

/// Resolves the live [`ModelSelection`] from the control plane: the same
/// resolution the inference owner routes the next request by.
pub trait ModelSelector: Send + Sync + 'static {
	/// The selection `con` makes now; `None` when it resolves to no model.
	fn select(&self, con: &omp_con::Ctx) -> Option<ModelSelection>;
}

/// Why the selected model or its thinking effort changed (Python
/// `ModelChangeReason`).
#[derive(Clone, Copy, Debug, Display, EnumString, Eq, IntoStaticStr, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ModelChangeReason {
	/// A user or client selected it (`/model`, the picker, `ai_thinking`,
	/// an RPC/ACP model command, a resumed session's journaled selection).
	User,
	/// The recovery middleware served the request on another model, or
	/// reverted to the selected one.
	Fallback,
	/// Role routing chose it: a role selector (`@plan`), a remapped role,
	/// or a Director bind (plan mode, prewalk).
	Role,
}

/// The control-plane variables whose writes can change the selection.
const WATCHED: [&str; 4] = [
	crate::AI_MODEL.name(),
	crate::AI_THINKING.name(),
	omp_catalog::settings::AI_MODEL_ROLES.name(),
	omp_ai::settings::AI_EXTERNAL_THINKING.name(),
];

/// What the watch last reported.
struct Watched {
	/// The last resolved selection.
	selected:         ModelSelection,
	/// The model requests are served on: the selection's, unless the
	/// recovery middleware moved them.
	serving:          Str,
	/// A Director bind supplied `ai_model`.
	model_engaged:    bool,
	/// A Director bind supplied `ai_thinking`.
	thinking_engaged: bool,
}

impl Watched {
	/// The selection `con` makes, reported as nothing.
	fn baseline(con: &omp_con::Ctx, selected: ModelSelection) -> Self {
		Self {
			serving: selected.model.clone(),
			model_engaged: con.engagement_owner(crate::AI_MODEL.name()).is_some(),
			thinking_engaged: con.engagement_owner(crate::AI_THINKING.name()).is_some(),
			selected,
		}
	}
}

/// The kernel-level `model_changed` emitter (see the module docs).
pub struct ModelWatch {
	hooks:    LifecycleHooks,
	selector: Arc<dyn ModelSelector>,
	state:    Mutex<Option<Watched>>,
}

impl ModelWatch {
	/// Watches `con` through `selector`, reporting on `hooks`. The selection
	/// `con` makes now is the baseline and raises nothing. The observer holds
	/// the watch weakly: dropping the returned handle ends the watch.
	#[must_use]
	pub fn attach(
		hooks: LifecycleHooks,
		selector: Arc<dyn ModelSelector>,
		con: &Arc<omp_con::Ctx>,
	) -> Arc<Self> {
		let state = selector
			.select(con)
			.map(|selected| Watched::baseline(con, selected));
		let watch = Arc::new(Self { hooks, selector, state: Mutex::new(state) });
		let weak_watch = Arc::downgrade(&watch);
		let weak_con: Weak<omp_con::Ctx> = Arc::downgrade(con);
		con.observe(move |name, _, _| {
			if !WATCHED.contains(&name) {
				return;
			}
			if let (Some(watch), Some(con)) = (weak_watch.upgrade(), weak_con.upgrade()) {
				watch.control_changed(&con, name);
			}
		});
		watch
	}

	/// A committed write to `var` may have changed the selection.
	///
	/// The change is role routing when a role remap made it, when a Director
	/// bind supplies (or until now supplied) the written variable, or when
	/// the new model comes from a role selector; otherwise it is the user's.
	fn control_changed(&self, con: &omp_con::Ctx, var: &str) {
		let Some(next) = self.selector.select(con) else {
			return;
		};
		let [model, thinking, roles, _] = WATCHED;
		let engaged = |name: &str| con.engagement_owner(name).is_some();
		let model_engaged = engaged(model);
		let thinking_engaged = engaged(thinking);
		let change = {
			let mut state = self.state.lock();
			let Some(watched) = state.as_mut() else {
				*state = Some(Watched::baseline(con, next));
				return;
			};
			let routed = var == roles
				|| (var == model && (model_engaged || watched.model_engaged || next.role.is_some()))
				|| (var == thinking && (thinking_engaged || watched.thinking_engaged));
			watched.model_engaged = model_engaged;
			watched.thinking_engaged = thinking_engaged;
			let model_changed = next.model != watched.selected.model;
			if !model_changed && next.thinking == watched.selected.thinking {
				watched.selected = next;
				return;
			}
			let from = watched.serving.clone();
			if model_changed {
				watched.serving = next.model.clone();
			}
			let change = Change {
				from,
				to: watched.serving.clone(),
				role: next.role.clone(),
				reason: if routed {
					ModelChangeReason::Role
				} else {
					ModelChangeReason::User
				},
				previous_thinking: watched.selected.thinking,
				thinking: next.thinking,
			};
			watched.selected = next;
			change
		};
		self.emit(&change);
	}

	/// A request's answer started on `model`: when the recovery middleware
	/// moved it off the model the watch last reported (a fallback, or the
	/// revert to the selection), that is a change.
	pub fn served(&self, model: &str) {
		let change = {
			let mut state = self.state.lock();
			let Some(watched) = state.as_mut() else {
				return;
			};
			if watched.serving == model {
				return;
			}
			let from = std::mem::replace(&mut watched.serving, Str::new(model));
			Change {
				from,
				to: watched.serving.clone(),
				role: watched.selected.role.clone(),
				reason: ModelChangeReason::Fallback,
				previous_thinking: watched.selected.thinking,
				thinking: watched.selected.thinking,
			}
		};
		self.emit(&change);
	}

	fn emit(&self, change: &Change) {
		let payload = ModelChanged {
			from_model:        Some(ModelRef::of(&change.from)),
			to_model:          ModelRef::of(&change.to),
			role:              change.role.as_deref().unwrap_or("default"),
			reason:            change.reason,
			previous_thinking: change.previous_thinking,
			thinking:          change.thinking,
		};
		let payload = match serde_json::to_value(&payload) {
			Ok(payload) => payload,
			Err(error) => {
				tracing::debug!(
					error = &error as &dyn std::error::Error,
					"model_changed payload not encoded"
				);
				return;
			},
		};
		if let Err(error) = self
			.hooks
			.notify(HookEventId::HookEventModelChanged, payload)
		{
			tracing::debug!(
				error = &error as &dyn std::error::Error,
				"model_changed observation not delivered"
			);
		}
	}
}

/// One transition the watch reports.
struct Change {
	from:              Str,
	to:                Str,
	role:              Option<Str>,
	reason:            ModelChangeReason,
	previous_thinking: Option<ReasoningEffort>,
	thinking:          Option<ReasoningEffort>,
}

/// The `model_changed` payload (Python `ModelChangedEvent`).
#[derive(Serialize)]
struct ModelChanged<'a> {
	from_model:        Option<ModelRef<'a>>,
	to_model:          ModelRef<'a>,
	role:              &'a str,
	reason:            ModelChangeReason,
	previous_thinking: Option<ReasoningEffort>,
	thinking:          Option<ReasoningEffort>,
}

/// Python `ModelRef` for a catalog key (`provider/model`).
#[derive(Serialize)]
struct ModelRef<'a> {
	provider: &'a str,
	api:      &'a str,
	model:    &'a str,
}

impl<'a> ModelRef<'a> {
	fn of(key: &'a str) -> Self {
		let (provider, model) = key.split_once('/').unwrap_or(("", key));
		Self { provider, api: "", model }
	}
}
