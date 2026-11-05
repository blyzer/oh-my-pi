//! Configuration for the shell output minimizer.
//!
//! The `sv_minimizer_*` convars are the settings surface; [`MinimizerConfig`]
//! is the resolved view the engine reads. None of them is settable from a
//! project cfg: a hostile repository could otherwise hide its own output or
//! install filters.

use std::{fs, io, path::PathBuf, sync::Arc};

use omp_con::Ctx;
use omp_core::{FastHashSet, Str};

use crate::pipeline::{self, PipelineError, PipelineRegistry};

const DEFAULT_MAX_CAPTURE_BYTES: u32 = 4 * 1024 * 1024;

omp_con::var! {
	/// Minimize shell output. Off by default; the other `sv_minimizer_*`
	/// settings are inert until this is on.
	pub static SV_MINIMIZER_ENABLED = sv_minimizer_enabled: bool {
		default: false,
		flags: archive,
	};
	/// Programs to minimize exclusively. Empty minimizes every program that
	/// has a filter.
	pub static SV_MINIMIZER_ONLY = sv_minimizer_only: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// Programs never minimized.
	pub static SV_MINIMIZER_EXCEPT = sv_minimizer_except: Vec<Str> {
		default: Vec::new(),
		flags: archive,
	};
	/// Captured bytes above which output passes through unminimized.
	pub static SV_MINIMIZER_MAX_CAPTURE_BYTES = sv_minimizer_max_capture_bytes: u32 {
		default: DEFAULT_MAX_CAPTURE_BYTES,
		min: 1024,
		flags: archive,
	};
	/// Kill switch: filters that shrink output by default keep their
	/// conservative path, and `&&`/`;` chains are never segmented.
	pub static SV_MINIMIZER_LEGACY_FILTERS = sv_minimizer_legacy_filters: bool {
		default: false,
		flags: archive,
	};
	/// TOML file of user filter definitions, in the schema of the built-in
	/// ones; they win over the built-ins. Empty loads none.
	pub static SV_MINIMIZER_FILTERS = sv_minimizer_filters: Str {
		default: Str::default(),
		flags: archive,
	};
}

/// Resolved minimizer configuration used by the engine.
#[derive(Debug, Clone)]
pub struct MinimizerConfig {
	/// Master switch.
	pub enabled:               bool,
	/// Lowercased programs to minimize exclusively; empty means all.
	pub only:                  FastHashSet<String>,
	/// Lowercased programs never minimized.
	pub except:                FastHashSet<String>,
	/// Captured bytes above which output passes through unminimized.
	pub max_capture_bytes:     u32,
	/// Compiled user-defined pipelines from `sv_minimizer_filters`. Searched
	/// before the built-in pipelines so user filters win.
	pub user_pipelines:        Option<Arc<PipelineRegistry>>,
	/// Resolved kill-switch: when true, opted-in filters return their
	/// conservative passthrough and chains are never segmented.
	pub legacy_filters_active: bool,
}

impl Default for MinimizerConfig {
	fn default() -> Self {
		Self {
			enabled:               false,
			only:                  FastHashSet::default(),
			except:                FastHashSet::default(),
			max_capture_bytes:     DEFAULT_MAX_CAPTURE_BYTES,
			user_pipelines:        None,
			legacy_filters_active: false,
		}
	}
}

/// A user filter definitions file that cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
	/// The file named by `sv_minimizer_filters` cannot be read.
	#[error("cannot read minimizer filters {}", path.display())]
	Read {
		/// The configured path.
		path:   PathBuf,
		/// The read failure.
		#[source]
		source: io::Error,
	},
	/// The file is not a valid set of filter definitions.
	#[error("invalid minimizer filters {}", path.display())]
	Definitions {
		/// The configured path.
		path:   PathBuf,
		/// What is wrong with the definitions.
		#[source]
		source: PipelineError,
	},
}

impl MinimizerConfig {
	/// Resolve the `sv_minimizer_*` convars. User filters are not loaded; see
	/// [`Self::load_user_filters`].
	#[must_use]
	pub fn from_ctx(ctx: &Ctx) -> Self {
		Self {
			enabled:               SV_MINIMIZER_ENABLED.get(ctx),
			only:                  lowercase_set(&SV_MINIMIZER_ONLY.get(ctx)),
			except:                lowercase_set(&SV_MINIMIZER_EXCEPT.get(ctx)),
			max_capture_bytes:     SV_MINIMIZER_MAX_CAPTURE_BYTES.get(ctx),
			user_pipelines:        None,
			legacy_filters_active: SV_MINIMIZER_LEGACY_FILTERS.get(ctx),
		}
	}

	/// Compile the user filter definitions named by `sv_minimizer_filters`.
	///
	/// An empty setting loads nothing. On error the configuration keeps its
	/// settings and the built-in filters, without user filters.
	pub fn load_user_filters(&mut self, ctx: &Ctx) -> Result<(), ConfigError> {
		let path = SV_MINIMIZER_FILTERS.get(ctx);
		if path.is_empty() {
			return Ok(());
		}
		let path = PathBuf::from(path.as_str());
		let contents = match fs::read_to_string(&path) {
			Ok(contents) => contents,
			Err(source) => return Err(ConfigError::Read { path, source }),
		};
		let (pipelines, tests) = match pipeline::parse_file(&contents, "user") {
			Ok(parsed) => parsed,
			Err(source) => return Err(ConfigError::Definitions { path, source }),
		};
		if !pipelines.is_empty() {
			self.user_pipelines = Some(Arc::new(PipelineRegistry { pipelines, tests }));
		}
		Ok(())
	}

	/// Whether the engine should attempt to minimize output for `program`.
	#[must_use]
	pub fn is_program_enabled(&self, program: &str) -> bool {
		if !self.enabled {
			return false;
		}
		let key = program.to_lowercase();
		if self.except.contains(&key) {
			return false;
		}
		if !self.only.is_empty() && !self.only.contains(&key) {
			return false;
		}
		true
	}

	/// Whether opted-in filters should fall back to their conservative path.
	#[must_use]
	pub const fn legacy_filters_active(&self) -> bool {
		self.legacy_filters_active
	}
}

fn lowercase_set(programs: &[Str]) -> FastHashSet<String> {
	programs
		.iter()
		.map(|program| program.to_lowercase())
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_are_disabled() {
		let cfg = MinimizerConfig::default();
		assert!(!cfg.enabled);
		assert!(!cfg.is_program_enabled("git"));
	}

	#[test]
	fn enabled_without_only_enables_any_program() {
		let ctx = Ctx::new();
		SV_MINIMIZER_ENABLED.set(&ctx, true).unwrap();
		let cfg = MinimizerConfig::from_ctx(&ctx);
		assert!(cfg.is_program_enabled("git"));
		assert!(cfg.is_program_enabled("cargo"));
	}

	#[test]
	fn only_list_is_respected() {
		let ctx = Ctx::new();
		SV_MINIMIZER_ENABLED.set(&ctx, true).unwrap();
		SV_MINIMIZER_ONLY
			.set(&ctx, vec![Str::new_static("git")])
			.unwrap();
		let cfg = MinimizerConfig::from_ctx(&ctx);
		assert!(cfg.is_program_enabled("git"));
		assert!(!cfg.is_program_enabled("cargo"));
	}

	#[test]
	fn except_overrides_only() {
		let ctx = Ctx::new();
		SV_MINIMIZER_ENABLED.set(&ctx, true).unwrap();
		SV_MINIMIZER_EXCEPT
			.set(&ctx, vec![Str::new_static("docker")])
			.unwrap();
		let cfg = MinimizerConfig::from_ctx(&ctx);
		assert!(!cfg.is_program_enabled("docker"));
		assert!(cfg.is_program_enabled("git"));
	}

	#[test]
	fn missing_filters_path_is_not_fatal() {
		let ctx = Ctx::new();
		SV_MINIMIZER_ENABLED.set(&ctx, true).unwrap();
		SV_MINIMIZER_FILTERS
			.set(&ctx, Str::new_static("/does/not/exist.toml"))
			.unwrap();
		let mut cfg = MinimizerConfig::from_ctx(&ctx);
		let err = cfg.load_user_filters(&ctx).unwrap_err();
		assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
		assert!(cfg.enabled);
	}

	#[test]
	fn legacy_filters_convar_true_sets_active() {
		let ctx = Ctx::new();
		SV_MINIMIZER_ENABLED.set(&ctx, true).unwrap();
		SV_MINIMIZER_LEGACY_FILTERS.set(&ctx, true).unwrap();
		assert!(MinimizerConfig::from_ctx(&ctx).legacy_filters_active());
	}

	#[test]
	fn legacy_filters_convar_false_keeps_inactive() {
		let ctx = Ctx::new();
		SV_MINIMIZER_ENABLED.set(&ctx, true).unwrap();
		SV_MINIMIZER_LEGACY_FILTERS.set(&ctx, false).unwrap();
		assert!(!MinimizerConfig::from_ctx(&ctx).legacy_filters_active());
	}

	#[test]
	fn legacy_filters_default_inactive() {
		assert!(!MinimizerConfig::from_ctx(&Ctx::new()).legacy_filters_active());
	}
}
