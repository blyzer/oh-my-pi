//! The adapter for this repository's `just` recipes over Cargo.

use std::{sync::Arc, time::Duration};

use omp_core::{Str, fmts_mut};

use crate::{
	adapter::{BuildSystemAdapter, PlanError, RepoPins},
	ids::{CacheName, ToolchainId},
	platform::{Need, NeedSet, PlatformRequirement, ResourceRequirements},
	request::{
		BuildCommand, CacheDecl, Eligibility, ExecutionPlan, ExecutionRequest, Intent, OutputSpec,
	},
};

/// How one `just` recipe may be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipeClass {
	/// The recipe name, `argv[1]`.
	pub recipe:        &'static str,
	/// What the recipe is for.
	pub intent:        Intent,
	/// Host capabilities it needs.
	pub needs:         &'static [Need],
	/// Whether it needs the network (cargo resolving crates).
	pub needs_network: bool,
	/// Where it may run on its host platform.
	pub eligibility:   Eligibility,
}

impl RecipeClass {
	/// Its needs as a set.
	pub fn need_set(&self) -> NeedSet {
		self.needs.iter().copied().collect()
	}
}

const fn recipe(
	recipe: &'static str,
	intent: Intent,
	needs: &'static [Need],
	needs_network: bool,
	eligibility: Eligibility,
) -> RecipeClass {
	RecipeClass { recipe, intent, needs, needs_network, eligibility }
}

/// The checked-in needs table for this repository's `justfile` recipes (ADR
/// 0040, Part C.1). A recipe not listed is [`Eligibility::LocalPreferred`].
///
/// `e2e-p7` needs a real PTY. It is `LocalRequired` because "`Pty` when no
/// backend offers one" holds for every backend slice 1 composes; it moves to
/// `RemotePreferred` once a backend advertises [`Need::Pty`], at which point
/// admission matches the need.
pub const CARGO_JUST_RECIPES: &[RecipeClass] = &[
	recipe("check", Intent::Check, &[], true, Eligibility::RemotePreferred),
	recipe("check-pkg", Intent::Check, &[], true, Eligibility::RemotePreferred),
	recipe("test-pkg", Intent::Test, &[], true, Eligibility::RemotePreferred),
	recipe("clippy", Intent::Lint, &[], true, Eligibility::RemotePreferred),
	recipe("e2e-core", Intent::Proof, &[], true, Eligibility::RemotePreferred),
	recipe("e2e-p9", Intent::Proof, &[], true, Eligibility::RemotePreferred),
	recipe("e2e-p10", Intent::Proof, &[], true, Eligibility::RemotePreferred),
	recipe("e2e-p11", Intent::Proof, &[], true, Eligibility::RemotePreferred),
	recipe("e2e-tool-sources", Intent::Proof, &[], true, Eligibility::RemotePreferred),
	recipe("e2e-p7", Intent::Proof, &[Need::Pty], true, Eligibility::LocalRequired),
	recipe("e2e-p12", Intent::Proof, &[Need::Seatbelt], true, Eligibility::LocalRequired),
	recipe("e2e-p8", Intent::Proof, &[Need::HostTiming], true, Eligibility::LocalRequired),
	recipe("e2e-baseline", Intent::Proof, &[Need::HostTiming], true, Eligibility::LocalRequired),
	recipe("fmt", Intent::Format, &[Need::WorkspaceWrite], false, Eligibility::LocalRequired),
	recipe("fmt-rust", Intent::Format, &[Need::WorkspaceWrite], false, Eligibility::LocalRequired),
	recipe("proto-fmt", Intent::Format, &[Need::WorkspaceWrite], false, Eligibility::LocalRequired),
];

/// Looks up `recipe` in [`CARGO_JUST_RECIPES`].
pub fn recipe_class(recipe: &str) -> Option<&'static RecipeClass> {
	CARGO_JUST_RECIPES
		.iter()
		.find(|class| class.recipe == recipe)
}

/// The program this adapter plans.
pub const JUST: &str = "just";

/// The named cache every Cargo recipe declares.
pub const CARGO_TARGET_CACHE: &str = "cargo-target";

/// Default wall-clock limit: 30 minutes, above the p90 of the whole macOS CI
/// job and the bound on leaked remote work when a cancel fails.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(30);

/// Plans `just <recipe> …` commands in this repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CargoJust {
	/// Wall-clock limit given to every request.
	pub timeout: Duration,
}

impl Default for CargoJust {
	fn default() -> Self {
		Self { timeout: DEFAULT_TIMEOUT }
	}
}

impl CargoJust {
	/// The toolchain id for `pins`:
	/// `rust:<channel>+nextest-<version>+cpython-<version>-<tag>`.
	pub fn toolchain(pins: &RepoPins) -> ToolchainId {
		ToolchainId::new(
			fmts_mut!(
				"rust:{}+nextest-{}+cpython-{}-{}",
				pins.rust_toolchain,
				pins.nextest,
				pins.cpython,
				pins.cpython_tag
			)
			.freeze(),
		)
	}
}

/// The working directory relative to the input root: `""` for the root, never
/// `"."`; absolute paths and `..` components are refused.
fn input_cwd(cwd: Option<&Str>) -> Result<Str, PlanError> {
	let Some(cwd) = cwd else {
		return Ok(Str::empty());
	};
	let text = cwd.as_str();
	if text.is_empty() || text == "." {
		return Ok(Str::empty());
	}
	if text.starts_with('/') || text.split('/').any(|component| component == "..") {
		return Err(PlanError::InvalidCwd { cwd: cwd.clone() });
	}
	Ok(cwd.clone())
}

impl BuildSystemAdapter for CargoJust {
	fn plan(&self, command: &BuildCommand, pins: &RepoPins) -> Result<ExecutionPlan, PlanError> {
		let Some(program) = command.argv.first() else {
			return Err(PlanError::EmptyArgv);
		};
		if program.as_str() != JUST {
			return Err(PlanError::UnsupportedProgram { program: program.clone() });
		}
		let cwd = input_cwd(command.cwd.as_ref())?;
		let class = command.argv.get(1).and_then(|recipe| recipe_class(recipe));
		let (intent, needs, needs_network, eligibility) = match class {
			Some(class) => (class.intent, class.need_set(), class.needs_network, class.eligibility),
			None => (command.intent, NeedSet::new(), true, Eligibility::LocalPreferred),
		};
		let eligibility = if command.target == command.host {
			eligibility
		} else {
			match eligibility {
				Eligibility::LocalRequired => {
					return Err(PlanError::HostOnly { platform: command.target });
				},
				Eligibility::RemoteRequired
				| Eligibility::RemotePreferred
				| Eligibility::LocalPreferred => Eligibility::RemoteRequired,
			}
		};
		Ok(ExecutionPlan::single(ExecutionRequest {
			id: command.id.clone(),
			intent,
			argv: command.argv.clone(),
			cwd,
			env: Arc::new([]),
			inputs: None,
			outputs: OutputSpec::Logs,
			toolchain: Self::toolchain(pins),
			caches: Arc::new([CacheDecl {
				name: CacheName::new_static(CARGO_TARGET_CACHE),
				env:  Str::new_static("CARGO_TARGET_DIR"),
			}]),
			platform: PlatformRequirement {
				os: command.target.os,
				arch: command.target.arch,
				needs_network,
				needs,
			},
			requirement: command.requirement,
			resources: ResourceRequirements::default(),
			eligibility,
			timeout: self.timeout,
		}))
	}
}

#[cfg(test)]
mod tests {
	use omp_adw::Requirement;

	use super::*;
	use crate::{
		backend::{
			BackendCapabilities, CachePolicy, CacheSupport, CancelSupport, ExecutionPolicy,
			FallbackPolicy,
		},
		error::Unplaceable,
		handle::ResumeSupport,
		ids::ExecutionId,
		place::{Placement, place},
		platform::Platform,
	};

	const JUSTFILE: &str = include_str!("../../../justfile");

	fn pins() -> RepoPins {
		RepoPins {
			rust_toolchain: Str::new_static("nightly-2026-08-08"),
			nextest:        Str::new_static("0.9.146"),
			cpython_tag:    Str::new_static("20260807"),
			cpython:        Str::new_static("3.14.7"),
		}
	}

	fn command(args: &[&'static str], target: Platform) -> BuildCommand {
		BuildCommand {
			id: ExecutionId::new_static("run-1"),
			argv: args.iter().copied().map(Str::new_static).collect(),
			cwd: None,
			intent: Intent::Other,
			requirement: Requirement::default(),
			target,
			host: Platform::MACOS_AARCH64,
		}
	}

	fn plan_one(args: &[&'static str]) -> ExecutionRequest {
		let plan = CargoJust::default()
			.plan(&command(args, Platform::MACOS_AARCH64), &pins())
			.expect("plan");
		assert_eq!(plan.requests.len(), 1);
		assert!(plan.edges.is_empty());
		plan.requests[0].clone()
	}

	/// A remote backend that offers every need, so only eligibility can keep a
	/// request off it.
	fn generous_backend(platform: Platform) -> BackendCapabilities {
		BackendCapabilities {
			platforms:      Arc::new([platform]),
			needs:          [
				Need::Pty,
				Need::Seatbelt,
				Need::Docker,
				Need::MacosSdk,
				Need::Gpu,
				Need::WorkspaceWrite,
				Need::HostTiming,
			]
			.into_iter()
			.collect(),
			max_timeout:    None,
			action_cache:   CacheSupport::None,
			cancel:         CancelSupport::Confirmed,
			resume:         ResumeSupport::Unsupported,
			max_blob_bytes: None,
			warm_caches:    false,
			enforced:       omp_adw::Posture {
				write:    omp_adw::WriteScope::ReadOnly,
				network:  omp_adw::NetworkScope::Unrestricted,
				approval: omp_adw::ApprovalScope::AlwaysAsk,
			},
		}
	}

	#[test]
	fn every_tabled_recipe_exists_in_the_justfile() {
		for class in CARGO_JUST_RECIPES {
			let defined = JUSTFILE.lines().any(|line| {
				line
					.strip_prefix(class.recipe)
					.is_some_and(|rest| rest.starts_with(':') || rest.starts_with(' '))
			});
			assert!(defined, "recipe {} is not defined in the justfile", class.recipe);
		}
	}

	#[test]
	fn e2e_p12_is_never_remote_eligible() {
		let request = plan_one(&["just", "e2e-p12"]);
		assert_eq!(request.eligibility, Eligibility::LocalRequired);
		assert!(request.platform.needs.contains(Need::Seatbelt));
		let backend = generous_backend(Platform::MACOS_AARCH64);
		for fallback in [
			FallbackPolicy::LocalOnly,
			FallbackPolicy::LocalPreferred,
			FallbackPolicy::RemotePreferred,
		] {
			assert_eq!(
				place(&request, &[&backend], &ExecutionPolicy { fallback, cache: CachePolicy::Off }),
				Ok(Placement::Local)
			);
		}
		assert_eq!(
			place(&request, &[&backend], &ExecutionPolicy {
				fallback: FallbackPolicy::RemoteOnly,
				cache:    CachePolicy::Off,
			}),
			Err(Unplaceable::LocalRequired)
		);
		assert_eq!(
			CargoJust::default().plan(&command(&["just", "e2e-p12"], Platform::LINUX_X86_64), &pins()),
			Err(PlanError::HostOnly { platform: Platform::LINUX_X86_64 })
		);
	}

	#[test]
	fn fmt_is_local_required_workspace_write() {
		let request = plan_one(&["just", "fmt"]);
		assert_eq!(request.eligibility, Eligibility::LocalRequired);
		assert_eq!(request.intent, Intent::Format);
		assert!(request.platform.needs.contains(Need::WorkspaceWrite));
	}

	#[test]
	fn host_bound_recipes_carry_their_needs() {
		let p7 = plan_one(&["just", "e2e-p7"]);
		assert!(p7.platform.needs.contains(Need::Pty));
		assert_eq!(p7.eligibility, Eligibility::LocalRequired);
		for recipe in ["e2e-p8", "e2e-baseline"] {
			let request = plan_one(&["just", recipe]);
			assert!(request.platform.needs.contains(Need::HostTiming), "{recipe}");
			assert_eq!(request.eligibility, Eligibility::LocalRequired, "{recipe}");
		}
	}

	#[test]
	fn linux_valid_recipes_are_remote_preferred_with_no_needs() {
		for args in [
			&["just", "test-pkg", "omp-envd"][..],
			&["just", "check-pkg", "omp-exec"],
			&["just", "clippy"],
			&["just", "e2e-core"],
		] {
			let request = plan_one(args);
			assert_eq!(request.eligibility, Eligibility::RemotePreferred, "{args:?}");
			assert!(request.platform.needs.is_empty(), "{args:?}");
		}
	}

	#[test]
	fn unknown_recipe_is_local_preferred() {
		for args in [&["just", "no-such-recipe"][..], &["just"], &["just", "--list"]] {
			let request = plan_one(args);
			assert_eq!(request.eligibility, Eligibility::LocalPreferred, "{args:?}");
			assert_eq!(request.intent, Intent::Other, "{args:?}");
		}
	}

	#[test]
	fn foreign_target_requires_remote() {
		let plan = CargoJust::default()
			.plan(&command(&["just", "test-pkg", "omp-envd"], Platform::LINUX_X86_64), &pins())
			.expect("plan");
		let request = &plan.requests[0];
		assert_eq!(request.eligibility, Eligibility::RemoteRequired);
		assert_eq!(request.platform.platform(), Platform::LINUX_X86_64);
	}

	#[test]
	fn plan_shapes_the_request() {
		let request = plan_one(&["just", "test-pkg", "omp-envd"]);
		assert_eq!(request.cwd, "");
		assert_eq!(
			request.toolchain.as_str(),
			"rust:nightly-2026-08-08+nextest-0.9.146+cpython-3.14.7-20260807"
		);
		assert_eq!(request.caches[0].name.as_str(), CARGO_TARGET_CACHE);
		assert_eq!(request.timeout, DEFAULT_TIMEOUT);
		assert!(request.inputs.is_none());
	}

	#[test]
	fn plan_refuses_other_programs_and_escaping_cwds() {
		let adapter = CargoJust::default();
		assert_eq!(
			adapter.plan(&command(&[], Platform::MACOS_AARCH64), &pins()),
			Err(PlanError::EmptyArgv)
		);
		assert_eq!(
			adapter.plan(&command(&["cargo", "test"], Platform::MACOS_AARCH64), &pins()),
			Err(PlanError::UnsupportedProgram { program: Str::new_static("cargo") })
		);
		let mut escaping = command(&["just", "clippy"], Platform::MACOS_AARCH64);
		escaping.cwd = Some(Str::new_static("../elsewhere"));
		assert!(matches!(adapter.plan(&escaping, &pins()), Err(PlanError::InvalidCwd { .. })));
		escaping.cwd = Some(Str::new_static("."));
		assert_eq!(adapter.plan(&escaping, &pins()).expect("plan").requests[0].cwd, "");
	}
}
