//! Remote Execution API v2 backend for [`omp_exec`] (ADR 0040, Slice 1 PR 1b).
//!
//! The crate speaks the standard REAPI over gRPC. Anything specific to one
//! service (the API-key header, platform property names, isolation and
//! recycling properties) is profile data supplied by the composing crate, and
//! the only vendor wire code is the invocation-cancel call behind the
//! `buildbuddy-cancel` feature.

/// Generated bindings for the vendored protos.
///
/// The module tree mirrors the protobuf package tree on purpose: prost emits
/// cross-package references as `super::super::...::google::rpc::Status`, so
/// `build` and `google` must be siblings under this module for those paths to
/// resolve.
pub mod proto {
	/// Pulls in the generated file for one protobuf package.
	macro_rules! generated {
		($file:literal) => {
			#![allow(
				missing_docs,
				clippy::pedantic,
				clippy::nursery,
				reason = "prost/tonic output is machine-generated and cannot follow handwritten \
				          documentation and style conventions"
			)]
			#![allow(
				clippy::allow_attributes_without_reason,
				reason = "prost/tonic emits compatibility allow attributes without Rust reason \
				          metadata"
			)]
			#![allow(
				clippy::large_enum_variant,
				reason = "prost maps protobuf oneofs directly to enums; boxing would change the \
				          generated Rust API"
			)]
			include!(concat!(env!("OUT_DIR"), "/proto/", $file));
		};
	}

	/// Packages under `build`.
	pub mod build {
		/// Packages under `build.bazel`.
		pub mod bazel {
			/// `build.bazel.semver`.
			pub mod semver {
				generated!("build.bazel.semver.rs");
			}
			/// Packages under `build.bazel.remote`.
			pub mod remote {
				/// Packages under `build.bazel.remote.execution`.
				pub mod execution {
					/// `build.bazel.remote.execution.v2`: CAS, action cache,
					/// execution and capabilities.
					pub mod v2 {
						generated!("build.bazel.remote.execution.v2.rs");
					}
				}
			}
		}
	}

	/// Packages under `google`.
	pub mod google {
		/// `google.api`.
		pub mod api {
			generated!("google.api.rs");
		}
		/// `google.bytestream`.
		pub mod bytestream {
			generated!("google.bytestream.rs");
		}
		/// `google.longrunning`.
		pub mod longrunning {
			generated!("google.longrunning.rs");
		}
		/// `google.rpc`.
		pub mod rpc {
			generated!("google.rpc.rs");
		}
	}
}
