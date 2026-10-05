//! Agent-owned product variables read by the kernel and its host surfaces
//! through the effective control plane.

use omp_core::Str;

/// Whether image attachments and `Read.question` media reach the model.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum VisionMode {
	/// Images flow when the route accepts image input, else they are
	/// replaced by their descriptions.
	#[default]
	Auto,
	/// Images always flow.
	On,
	/// Images are always replaced by their descriptions.
	Off,
}

omp_con::con_enum!(VisionMode);

/// How context compaction condenses the history it hides.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum CompactionStrategy {
	/// The active model writes a textual handoff summary.
	#[default]
	Soft,
	/// The hidden history is rendered verbatim into PNG frames the model
	/// reads as images; routes without image input fall back to `soft`.
	Snapcompact,
}

omp_con::con_enum!(CompactionStrategy);

/// When a matching stream rule stops generation.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "kebab-case")]
pub enum StreamRuleInterrupt {
	/// Interrupt for prose and tool argument matches.
	#[default]
	Always,
	/// Interrupt for text and thinking only.
	ProseOnly,
	/// Interrupt for tool argument matches only.
	ToolOnly,
	/// Deliver reminders without stopping generation.
	Never,
}

omp_con::con_enum!(StreamRuleInterrupt);

/// Whether interrupted assistant output stays in model context.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "kebab-case")]
pub enum StreamRuleContext {
	/// Exclude the partial assistant response.
	#[default]
	Discard,
	/// Keep the partial assistant response.
	Keep,
}

omp_con::con_enum!(StreamRuleContext);

/// How often a stream rule may fire in a session.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "kebab-case")]
pub enum StreamRuleRepeat {
	/// Fire at most once per session.
	#[default]
	Once,
	/// Fire again after the configured response gap.
	AfterGap,
}

omp_con::con_enum!(StreamRuleRepeat);

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
		flags: session,
	};
	/// Model route for task subagents; empty inherits `ai_model`. It seeds a
	/// child's `ai_model` before `subagent.cfg` and the agent class cfg run,
	/// so an `ai_model` either cfg sets (including `inherit`, which picks the
	/// spawning session's model instead), and a
	/// `sv_task_agent_model_overrides` entry for the class, all take
	/// precedence over it.
	pub static AI_TASK_MODEL = ai_task_model: Str {
		default: Str::new_static(""),
		flags: archive | session,
	};
	/// Selected model reasoning level (`off`, `minimal`, `low`, `medium`,
	/// `high`, `xhigh`, `max`). Routes that cannot honor a level clamp it
	/// (ADR 0017).
	pub static AI_THINKING = ai_thinking: Str {
		default: Str::new_static("high"),
		suggest: ["off", "minimal", "low", "medium", "high", "xhigh", "max"],
		flags: archive | session,
	};
	/// Enables the low-latency model path.
	pub static AI_FASTMODE = ai_fastmode: bool {
		default: false,
		flags: archive | session,
	};
	/// Controls the inspect_image tool, which delegates image understanding to
	/// a vision-capable model. `auto` exposes it only when the active model
	/// lacks native image input; `on` always exposes it; `off` never does.
	pub static AI_VISION = ai_vision: VisionMode {
		default: VisionMode::Auto,
		flags: archive | session,
		meta: {
			"ui.tab": "tools",
			"ui.group": "Available Tools",
			"ui.label": "Inspect Image",
			"ui.option.auto": "Auto (only for models without vision)",
			"ui.option.on": "On",
			"ui.option.off": "Off",
			"legacy.path": "inspect_image.mode",
		},
	};
	/// Mode prompt rendered into the system prompt while engaged: names
	/// `prompts/modes/<name>.md` (`plan`, `vibe`, `autoresearch`); empty
	/// renders none. Bound by Directors, so the value derives from the live
	/// `<meta><directors>` stack (ADR 0015).
	pub static AI_PROMPT_MODE = ai_prompt_mode: Str {
		default: Str::new_static(""),
		suggest: ["plan", "vibe", "autoresearch"],
		flags: session,
	};
	/// How automatic and bare `/compact` compaction condenses hidden history:
	/// `soft` summarizes with the active model; `snapcompact` archives it
	/// verbatim as image frames (vision routes only, else `soft`).
	pub static AI_COMPACTION_STRATEGY = ai_compaction_strategy: CompactionStrategy {
		default: CompactionStrategy::Soft,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Compaction",
			"ui.label": "Compaction Strategy",
			"ui.option.soft": "Soft (LLM summary)",
			"ui.option.snapcompact": "Snapcompact (image archive; vision models)",
		},
	};
	/// Context-window fraction at which context maintenance begins.
	pub static AI_COMPACT_THRESHOLD = ai_compact_threshold: f64 {
		default: 0.80,
		min: 0.0,
		max: 1.0,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Compaction",
			"ui.label": "Compaction Threshold",
			"ui.unit": "percent",
			"ui.option.0.1": "10%",
			"ui.option.0.1.desc": "Extremely early maintenance",
			"ui.option.0.2": "20%",
			"ui.option.0.2.desc": "Very early maintenance",
			"ui.option.0.3": "30%",
			"ui.option.0.3.desc": "Early maintenance",
			"ui.option.0.4": "40%",
			"ui.option.0.4.desc": "Moderately early maintenance",
			"ui.option.0.5": "50%",
			"ui.option.0.5.desc": "Halfway point",
			"ui.option.0.6": "60%",
			"ui.option.0.6.desc": "Moderate context usage",
			"ui.option.0.7": "70%",
			"ui.option.0.7.desc": "Balanced",
			"ui.option.0.75": "75%",
			"ui.option.0.75.desc": "Slightly aggressive",
			"ui.option.0.8": "80%",
			"ui.option.0.8.desc": "Typical threshold",
			"ui.option.0.85": "85%",
			"ui.option.0.85.desc": "Aggressive context usage",
			"ui.option.0.9": "90%",
			"ui.option.0.9.desc": "Very aggressive",
			"ui.option.0.95": "95%",
			"ui.option.0.95.desc": "Near context limit",
			"legacy.path": "compaction.thresholdPercent",
		},
	};
	/// List available skills in the system prompt; disable to save context and
	/// toggle per-session with /skillful.
	pub static AI_SKILLFUL = ai_skillful: bool {
		default: true,
		flags: archive | session,
		meta: {
			"ui.tab": "model",
			"ui.group": "Prompt",
			"ui.label": "List Skills in Prompt",
			"legacy.path": "skillful",
		},
	};
	/// Host approval policy.
	pub static SV_APPROVAL_MODE = sv_approval_mode: Str {
		default: Str::new_static("on-request"),
		flags: archive | session | replicated,
	};
	/// Tool allowlist: stable tool names the model may call (`--tools`, Director
	/// binds such as Vibe's `[read todo]`). The kernel snapshots it per request
	/// and refuses any other call at dispatch (`tool.roster.restricted`). The
	/// value the session is composed with also narrows the advertised roster;
	/// that roster is latched at the first request (ADR 0024), so later writes
	/// and Director binds change what may run, never what is advertised. Empty
	/// allows every registered tool.
	pub static SV_TOOLS = sv_tools: Vec<Str> {
		default: Vec::new(),
		flags: archive | session | replicated,
	};
	/// Read-only ceiling of a subagent spawned under plan mode: only plan
	/// mode's read-only tools may run, whatever `sv_tools` allows, and the
	/// environment refuses every write the subagent's calls attempt. The host
	/// sets it when it spawns a child of a plan-mode session; the child's own
	/// subagents inherit it. Scripts, cfgs, and the console cannot write it.
	pub static SV_TOOLS_READ_ONLY = sv_tools_read_only: bool {
		default: false,
		flags: readonly,
	};
	/// Enables stream-time rule matching.
	pub static AI_STREAM_RULES_ENABLED = ai_stream_rules_enabled: bool {
		default: true,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Stream Rules",
			"ui.label": "Stream Rules",
			"legacy.path": "ttsr.enabled",
		},
	};
	/// Interrupt policy for matching stream rules.
	pub static AI_STREAM_RULES_INTERRUPT = ai_stream_rules_interrupt: StreamRuleInterrupt {
		default: StreamRuleInterrupt::Always,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Stream Rules",
			"ui.label": "Interrupt",
			"legacy.path": "ttsr.interruptMode",
		},
	};
	/// Context policy for interrupted stream-rule responses.
	pub static AI_STREAM_RULES_CONTEXT = ai_stream_rules_context: StreamRuleContext {
		default: StreamRuleContext::Discard,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Stream Rules",
			"ui.label": "Interrupted Output",
			"legacy.path": "ttsr.contextMode",
		},
	};
	/// Repeat policy for stream rules.
	pub static AI_STREAM_RULES_REPEAT = ai_stream_rules_repeat: StreamRuleRepeat {
		default: StreamRuleRepeat::Once,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Stream Rules",
			"ui.label": "Repeat",
			"legacy.path": "ttsr.repeatMode",
		},
	};
	/// Completed assistant responses required between repeated rule matches.
	pub static AI_STREAM_RULES_REPEAT_GAP = ai_stream_rules_repeat_gap: u32 {
		default: 10,
		min: 0,
		max: 1000,
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Stream Rules",
			"ui.label": "Repeat Gap",
			"legacy.path": "ttsr.repeatGap",
		},
	};
	/// Rule names excluded from stream matching.
	pub static AI_STREAM_RULES_DISABLED = ai_stream_rules_disabled: Vec<Str> {
		default: Vec::new(),
		flags: archive | session,
		meta: {
			"ui.tab": "context",
			"ui.group": "Stream Rules",
			"ui.label": "Disabled Rules",
			"legacy.path": "ttsr.disabledRules",
		},
	};
}

/// How many queued steering asides one safe point consumes.
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SteeringMode {
	/// One interjection per safe point; the rest wait for the next one.
	#[default]
	OneAtATime,
	/// Every queued interjection lands at the first safe point.
	All,
}

omp_con::con_enum!(SteeringMode);

omp_con::var! {
	/// How to process queued messages while the agent is working.
	pub static AI_STEERING_MODE = ai_steering_mode: SteeringMode {
		default: SteeringMode::OneAtATime,
		flags: archive | session,
		meta: {
			"ui.tab": "interaction",
			"ui.group": "Input",
			"ui.label": "Steering Mode",
			"legacy.path": "steeringMode",
		},
	};
}

/// Tool names the effective `sv_tools` allowlist lets a call use (a Director's
/// bind included); `None` means every registered tool.
#[must_use]
pub fn tool_allowlist(con: Option<&omp_con::Ctx>) -> Option<Vec<Str>> {
	let roster = SV_TOOLS.get(con?);
	(!roster.is_empty()).then_some(roster)
}

/// The `sv_tools` allowlist the session was composed with (`--tools`, agent
/// cfg), which narrows the latched wire roster: the value beneath every
/// Director bind, so a session composed or resumed while Plan or Vibe is
/// engaged still advertises its full roster. `None` means every registered
/// tool.
#[must_use]
pub fn composed_tool_allowlist(con: Option<&omp_con::Ctx>) -> Option<Vec<Str>> {
	let value = con?.value_below_engagements(SV_TOOLS.name()).ok()?;
	let roster = <Vec<Str> as omp_con::ConType>::from_value(&value)?;
	(!roster.is_empty()).then_some(roster)
}
