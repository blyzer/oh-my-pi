//! Control-plane variables shared by the kernel and the environment host.

use omp_core::Str;

/// The `ai_model` value an agent class cfg (or `subagent.cfg`) sets to run
/// its children on the spawning session's own model, ahead of
/// `ai_task_model`.
pub const AI_MODEL_INHERIT: &str = "inherit";

omp_con::var! {
	/// The session's live model route. Journaled with the session, never
	/// archived: the remembered default model is `ai_model_roles.default`,
	/// so a picker choice saved to `config.cfg` cannot outrank it.
	///
	/// In a subagent's `subagent.cfg` or agent class cfg, the value the cfgs
	/// leave decides the child's route: a selector is the class's own model;
	/// `inherit` ([`AI_MODEL_INHERIT`]) is the spawning session's model,
	/// skipping `ai_task_model`; empty (`reset ai_model`) is no class model,
	/// so `ai_task_model` applies, else the spawning session's model.
	pub static AI_MODEL = ai_model: Str {
		default: Str::new_static(""),
		flags: session | project,
	};
}
