//! Pure placement of one request over backend capabilities and the operator
//! policy (ADR 0040, Part C.1-C.3).
//!
//! Slice 1 has no fallback: placement picks once and a remote failure is
//! reported, never retried elsewhere.

use omp_adw::{NetworkScope, PhaseName};

use crate::{
	backend::{BackendCapabilities, ExecutionPolicy, FallbackPolicy},
	error::{Rejection, Unplaceable},
	request::{Eligibility, ExecutionRequest},
};

/// Where a request runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placement {
	/// On this host, through the caller's existing local path.
	Local,
	/// On the offered backend at `backend` (an index into the slice given to
	/// [`place`]).
	Remote {
		/// Index of the admitting backend.
		backend: usize,
	},
}

/// Places `request` on this host or on the first offered remote backend that
/// admits it.
///
/// `backends` are the remote backends the operator configured, in preference
/// order. The policy decides whether the request goes remote at all:
///
/// | Eligibility | `local-only` | `local-preferred` | `remote-preferred` | `remote-only` |
/// |---|---|---|---|---|
/// | `LocalRequired` | local | local | local | unplaceable |
/// | `RemoteRequired` | unplaceable | remote | remote | remote |
/// | `RemotePreferred` | local | local | remote | remote |
/// | `LocalPreferred` | local | local | local | local |
///
/// `LocalPreferred` stays local under `remote-only` because slice 1 forces
/// only adapter-eligible work remote and keeps every other phase on today's
/// local path. A request that goes remote and that no backend admits is
/// [`Unplaceable::NoBackend`]; it never falls back to local.
///
/// # Errors
///
/// Returns [`Unplaceable`] when the policy forbids every placement the
/// request's eligibility allows, or when no offered backend admits it.
pub fn place(
	request: &ExecutionRequest,
	backends: &[&BackendCapabilities],
	policy: &ExecutionPolicy,
) -> Result<Placement, Unplaceable> {
	let remote = match (request.eligibility, policy.fallback) {
		(Eligibility::LocalRequired, FallbackPolicy::RemoteOnly) => {
			return Err(Unplaceable::LocalRequired);
		},
		(Eligibility::RemoteRequired, FallbackPolicy::LocalOnly) => {
			return Err(Unplaceable::RemoteRequired);
		},
		(
			Eligibility::LocalRequired,
			FallbackPolicy::LocalOnly
			| FallbackPolicy::LocalPreferred
			| FallbackPolicy::RemotePreferred,
		)
		| (
			Eligibility::RemotePreferred,
			FallbackPolicy::LocalOnly | FallbackPolicy::LocalPreferred,
		)
		| (Eligibility::LocalPreferred, _) => false,
		(
			Eligibility::RemoteRequired,
			FallbackPolicy::LocalPreferred
			| FallbackPolicy::RemotePreferred
			| FallbackPolicy::RemoteOnly,
		)
		| (
			Eligibility::RemotePreferred,
			FallbackPolicy::RemotePreferred | FallbackPolicy::RemoteOnly,
		) => true,
	};
	if !remote {
		return Ok(Placement::Local);
	}
	if let Some(backend) = backends
		.iter()
		.position(|backend| admit(request, backend).is_ok())
	{
		return Ok(Placement::Remote { backend });
	}
	Err(Unplaceable::NoBackend {
		rejections: backends
			.iter()
			.filter_map(|backend| admit(request, backend).err())
			.collect(),
	})
}

/// Whether `backend` may run `request`.
///
/// Admission is per placement and uses what the backend *enforces*: the
/// platform must be offered, every need provided, the network enforced on if
/// the command needs it, and the enforced posture must satisfy the request's
/// requirement ([`omp_adw::Requirement::check`]). A posture failure names the
/// request's execution id where the check expects a phase.
///
/// # Errors
///
/// Returns the first [`Rejection`] in that order.
pub fn admit(request: &ExecutionRequest, backend: &BackendCapabilities) -> Result<(), Rejection> {
	let platform = request.platform.platform();
	if !backend.platforms.contains(&platform) {
		return Err(Rejection::Platform { platform });
	}
	if let Some(need) = request.platform.needs.difference(&backend.needs).first() {
		return Err(Rejection::Need { need });
	}
	if request.platform.needs_network && backend.enforced.network == NetworkScope::Disabled {
		return Err(Rejection::Network);
	}
	request
		.requirement
		.check(PhaseName::from_ref(request.id.as_str()), &backend.enforced)
		.map_err(|error| Rejection::Posture { error })
}

#[cfg(test)]
mod tests {
	use std::{sync::Arc, time::Duration};

	use omp_adw::{ApprovalScope, Posture, Requirement, WriteScope};
	use omp_core::Str;

	use super::*;
	use crate::{
		backend::{CachePolicy, CacheSupport, CancelSupport},
		ids::{ExecutionId, ToolchainId},
		platform::{Need, NeedSet, Platform, PlatformRequirement, ResourceRequirements},
		request::{Intent, OutputSpec},
	};

	fn request(eligibility: Eligibility) -> ExecutionRequest {
		ExecutionRequest {
			id: ExecutionId::new_static("run-1"),
			intent: Intent::Test,
			argv: Arc::new([Str::new_static("just"), Str::new_static("test-pkg")]),
			cwd: Str::empty(),
			env: Arc::new([]),
			inputs: None,
			outputs: OutputSpec::Logs,
			toolchain: ToolchainId::new_static("rust:test"),
			caches: Arc::new([]),
			platform: PlatformRequirement {
				os:            Platform::LINUX_X86_64.os,
				arch:          Platform::LINUX_X86_64.arch,
				needs_network: true,
				needs:         NeedSet::new(),
			},
			requirement: Requirement::default(),
			resources: ResourceRequirements::default(),
			eligibility,
			timeout: Duration::from_secs(60),
		}
	}

	fn linux_backend() -> BackendCapabilities {
		BackendCapabilities {
			platforms:      Arc::new([Platform::LINUX_X86_64]),
			needs:          NeedSet::new(),
			max_timeout:    None,
			action_cache:   CacheSupport::None,
			cancel:         CancelSupport::BestEffort,
			max_blob_bytes: None,
			warm_caches:    false,
			enforced:       Posture {
				write:    WriteScope::WorkspaceWrite,
				network:  NetworkScope::Unrestricted,
				approval: ApprovalScope::Yolo,
			},
		}
	}

	const fn policy(fallback: FallbackPolicy) -> ExecutionPolicy {
		ExecutionPolicy { fallback, cache: CachePolicy::Off }
	}

	#[test]
	fn local_required_is_refused_under_remote_only() {
		let backend = linux_backend();
		assert_eq!(
			place(
				&request(Eligibility::LocalRequired),
				&[&backend],
				&policy(FallbackPolicy::RemoteOnly)
			),
			Err(Unplaceable::LocalRequired)
		);
	}

	#[test]
	fn local_required_never_goes_remote() {
		let backend = linux_backend();
		for fallback in [
			FallbackPolicy::LocalOnly,
			FallbackPolicy::LocalPreferred,
			FallbackPolicy::RemotePreferred,
		] {
			assert_eq!(
				place(&request(Eligibility::LocalRequired), &[&backend], &policy(fallback)),
				Ok(Placement::Local),
				"{fallback}"
			);
		}
	}

	#[test]
	fn remote_required_without_an_admitting_backend_is_unplaceable() {
		let mut macos = linux_backend();
		macos.platforms = Arc::new([Platform::MACOS_AARCH64]);
		let mut offline = linux_backend();
		offline.enforced.network = NetworkScope::Disabled;
		let request = request(Eligibility::RemoteRequired);
		for fallback in [
			FallbackPolicy::LocalPreferred,
			FallbackPolicy::RemotePreferred,
			FallbackPolicy::RemoteOnly,
		] {
			assert_eq!(
				place(&request, &[&macos, &offline], &policy(fallback)),
				Err(Unplaceable::NoBackend {
					rejections: Arc::new([
						Rejection::Platform { platform: Platform::LINUX_X86_64 },
						Rejection::Network,
					]),
				}),
				"{fallback}"
			);
			assert_eq!(
				place(&request, &[], &policy(fallback)),
				Err(Unplaceable::NoBackend { rejections: Arc::new([]) }),
				"{fallback}"
			);
		}
		assert_eq!(
			place(&request, &[&linux_backend()], &policy(FallbackPolicy::LocalOnly)),
			Err(Unplaceable::RemoteRequired)
		);
	}

	#[test]
	fn remote_preferred_is_forced_remote_only_by_a_remote_policy() {
		let backend = linux_backend();
		let request = request(Eligibility::RemotePreferred);
		assert_eq!(
			place(&request, &[&backend], &policy(FallbackPolicy::RemoteOnly)),
			Ok(Placement::Remote { backend: 0 })
		);
		assert_eq!(
			place(&request, &[&backend], &policy(FallbackPolicy::LocalOnly)),
			Ok(Placement::Local)
		);
	}

	#[test]
	fn local_preferred_stays_local_under_every_policy() {
		let backend = linux_backend();
		for fallback in [
			FallbackPolicy::LocalOnly,
			FallbackPolicy::LocalPreferred,
			FallbackPolicy::RemotePreferred,
			FallbackPolicy::RemoteOnly,
		] {
			assert_eq!(
				place(&request(Eligibility::LocalPreferred), &[&backend], &policy(fallback)),
				Ok(Placement::Local),
				"{fallback}"
			);
		}
	}

	#[test]
	fn admission_checks_the_enforced_posture_and_needs() {
		let loose = linux_backend();
		let mut strict = linux_backend();
		strict.enforced.write = WriteScope::ReadOnly;
		let mut request = request(Eligibility::RemotePreferred);
		request.requirement = Requirement::default().writing(WriteScope::ReadOnly);
		assert!(matches!(admit(&request, &loose), Err(Rejection::Posture { .. })));
		assert_eq!(
			place(&request, &[&loose, &strict], &policy(FallbackPolicy::RemoteOnly)),
			Ok(Placement::Remote { backend: 1 })
		);

		request.platform.needs.insert(Need::Pty);
		assert_eq!(admit(&request, &strict), Err(Rejection::Need { need: Need::Pty }));
		strict.needs.insert(Need::Pty);
		assert_eq!(admit(&request, &strict), Ok(()));
	}
	mod proptests {
		use proptest::prelude::*;

		use super::*;
		use crate::{
			CasOp, Digest, ExecutionError, ForeignError, RemoteCancel, Stage, Unplaceable,
			fallback_eligible,
		};

		fn any_eligibility() -> impl Strategy<Value = Eligibility> {
			prop_oneof![
				Just(Eligibility::LocalRequired),
				Just(Eligibility::LocalPreferred),
				Just(Eligibility::RemotePreferred),
				Just(Eligibility::RemoteRequired),
			]
		}

		fn any_fallback_policy() -> impl Strategy<Value = FallbackPolicy> {
			prop_oneof![
				Just(FallbackPolicy::LocalOnly),
				Just(FallbackPolicy::LocalPreferred),
				Just(FallbackPolicy::RemotePreferred),
				Just(FallbackPolicy::RemoteOnly),
			]
		}

		fn any_execution_error() -> impl Strategy<Value = ExecutionError> {
			use std::sync::Arc;
			use omp_core::{Hash32, Str};

			#[derive(Debug)]
			struct Cause;
			impl std::fmt::Display for Cause {
				fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
					write!(f, "cause")
				}
			}
			impl std::error::Error for Cause {}
			fn cause() -> ForeignError { Arc::new(Cause) }
			fn digest() -> Digest { Digest::new(Hash32::default(), 0) }

			prop_oneof![
				Just(ExecutionError::Unplaceable {
					need: crate::Unplaceable::LocalRequired,
				}),
				Just(ExecutionError::Auth {
					backend: crate::BackendId::new_static("b"),
				}),
				prop_oneof![
					Just(CasOp::FindMissing),
					Just(CasOp::Upload),
					Just(CasOp::Download),
				].prop_map(|op| ExecutionError::Cas { op, digest: digest() }),
				Just(ExecutionError::Capacity {
					backend: crate::BackendId::new_static("b"),
				}),
				Just(ExecutionError::TimedOut {
					limit: std::time::Duration::from_secs(1),
				}),
				prop_oneof![
					Just(RemoteCancel::NotStarted),
					Just(RemoteCancel::Confirmed),
					Just(RemoteCancel::Requested),
					Just(RemoteCancel::Failed),
					Just(RemoteCancel::Unsupported),
				].prop_map(|remote| ExecutionError::Cancelled { remote }),
				Just(ExecutionError::Transport { source: cause() }),
				Just(ExecutionError::Infrastructure { source: cause() }),
				Just(ExecutionError::InputCapture { source: cause() }),
				Just(ExecutionError::InputRefused {
					path: Str::new_static("a"),
					rule: Str::new_static("r"),
				}),
				Just(ExecutionError::OutputRetrieval { source: cause() }),
				Just(ExecutionError::Config { source: cause() }),
			]
		}

		proptest! {
			/// `LocalRequired` is never placed remote.
			#[test]
			fn local_required_never_remote(policy in any_fallback_policy()) {
				let backend = linux_backend();
				let result = place(
					&request(Eligibility::LocalRequired),
					&[&backend],
					&ExecutionPolicy { fallback: policy, cache: CachePolicy::Off },
				);
				match result {
					Ok(Placement::Remote { .. }) => {
						panic!("LocalRequired must never be placed remote (policy={policy})");
					},
					Ok(Placement::Local) | Err(_) => {},
				}
			}

			/// `RemoteRequired` is never placed local when any backend admits it.
			#[test]
			fn remote_required_never_local_when_backend_admits(policy in any_fallback_policy()) {
				let backend = linux_backend();
				let result = place(
					&request(Eligibility::RemoteRequired),
					&[&backend],
					&ExecutionPolicy { fallback: policy, cache: CachePolicy::Off },
				);
				match result {
					Ok(Placement::Local) => {
						panic!("RemoteRequired must never be placed local (policy={policy})");
					},
					Ok(Placement::Remote { .. }) | Err(_) => {},
				}
			}

			/// No failure after acceptance is fallback-eligible (never rerun).
			#[test]
			fn no_post_acceptance_fallback(error in any_execution_error()) {
				prop_assert!(
					!fallback_eligible(&error, Stage::AfterAcceptance),
					"post-acceptance error must never be fallback-eligible: {error:?}",
				);
			}

			/// Cancelled is never fallback-eligible.
			#[test]
			fn cancelled_never_fallback_eligible(
				remote in prop_oneof![
					Just(RemoteCancel::NotStarted),
					Just(RemoteCancel::Confirmed),
					Just(RemoteCancel::Requested),
					Just(RemoteCancel::Failed),
					Just(RemoteCancel::Unsupported),
				],
				stage in prop_oneof![Just(Stage::BeforeAcceptance), Just(Stage::AfterAcceptance)],
			) {
				let error = ExecutionError::Cancelled { remote };
				prop_assert!(
					!fallback_eligible(&error, stage),
					"Cancelled must never be fallback-eligible",
				);
			}

			/// All four policies: eligibility and placement are consistent.
			#[test]
			fn placement_respects_eligibility_and_policy(
				eligibility in any_eligibility(),
				policy in any_fallback_policy(),
			) {
				let backend = linux_backend();
				let result = place(
					&request(eligibility),
					&[&backend],
					&ExecutionPolicy { fallback: policy, cache: CachePolicy::Off },
				);
				match (eligibility, policy, result) {
					(Eligibility::LocalRequired, FallbackPolicy::RemoteOnly, Err(Unplaceable::LocalRequired)) => {},
					(Eligibility::LocalRequired, _, Ok(Placement::Local)) => {},
					(Eligibility::RemoteRequired, FallbackPolicy::LocalOnly, Err(Unplaceable::RemoteRequired)) => {},
					(Eligibility::RemoteRequired, _, Ok(Placement::Remote { .. })) => {},
					(Eligibility::RemotePreferred, FallbackPolicy::LocalOnly | FallbackPolicy::LocalPreferred, Ok(Placement::Local)) => {},
					(Eligibility::RemotePreferred, FallbackPolicy::RemotePreferred | FallbackPolicy::RemoteOnly, Ok(Placement::Remote { .. })) => {},
					(Eligibility::LocalPreferred, _, Ok(Placement::Local)) => {},
					(e, p, r) => panic!("unexpected placement: eligibility={e:?} policy={p:?} result={r:?}"),
				}
			}
		}
	}
}
