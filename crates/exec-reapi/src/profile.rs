//! Service profiles: everything that differs between REAPI servers, as data.
//!
//! The client never branches on a vendor name. A [`Profile`] says which
//! metadata header carries the credential, how the platform is spelled, and
//! which extra properties a request gets; the client applies those facts and
//! nothing else. Adding a service is a new profile value, not new client code
//! (ADR 0040, Part A.3).

use std::{collections::BTreeMap, fmt};

use omp_core::{ExposeSecret, SecretString, Str};
use omp_exec::{Arch, Os, Platform};
use strum::{Display, EnumString, IntoStaticStr};

/// A credential destined for one gRPC metadata header and nowhere else.
///
/// It is wiped on drop, redacted in `Debug`, and has no `Display` or serde
/// implementation, so it cannot reach argv, an action environment, a log line
/// or a journal by accident (ADR 0040, Part F). [`Credential::expose`] is the
/// one way out and the only caller is the metadata writer.
#[derive(Clone)]
pub struct Credential(SecretString);

impl Credential {
	/// Wraps a credential read from its configured source.
	pub fn new(secret: impl Into<SecretString>) -> Self {
		Self(secret.into())
	}

	/// The plaintext, for writing into the one metadata header.
	pub fn expose(&self) -> &str {
		self.0.expose_secret()
	}
}

impl fmt::Debug for Credential {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("Credential([REDACTED])")
	}
}

/// How a service spells the architecture in a platform property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum ArchSpelling {
	/// REAPI's own vocabulary: `ISA` with `x86-64` or `arm64`.
	Reapi,
	/// BuildBuddy's vocabulary: `Arch` with `amd64` or `arm64`.
	Short,
}

/// The facts that differ between REAPI servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
	/// The profile name an operator selects.
	pub name:              Str,
	/// The metadata header that carries the credential, if the service wants
	/// one. A server without authentication has none.
	pub credential_header: Option<Str>,
	/// How the architecture property is spelled.
	pub arch:              ArchSpelling,
	/// The platform property that names the container image a toolchain maps
	/// to. The image reference itself is operator data
	/// ([`crate::Toolchain`]); only the property's spelling is a service fact.
	pub image_property:    Str,
	/// Properties added to every action, in deterministic order.
	pub fixed:             BTreeMap<Str, Str>,
}

impl Profile {
	/// The portable profile: standard REAPI vocabulary, no credential header,
	/// no vendor properties.
	pub fn reapi() -> Self {
		Self {
			name:              Str::new_static("reapi"),
			credential_header: None,
			arch:              ArchSpelling::Reapi,
			image_property:    Str::new_static("container-image"),
			fixed:             BTreeMap::new(),
		}
	}

	/// BuildBuddy: the API-key header and its platform vocabulary. Runner
	/// recycling is deliberately absent: the 2026-10-10 probe showed a raw REAPI
	/// action does not get a warm runner, so no property is sent that implies
	/// one (ADR 0040, amendment 2026-10-10).
	pub fn buildbuddy() -> Self {
		Self {
			name:              Str::new_static("buildbuddy"),
			credential_header: Some(Str::new_static("x-buildbuddy-api-key")),
			arch:              ArchSpelling::Short,
			image_property:    Str::new_static("container-image"),
			fixed:             BTreeMap::from([(
				Str::new_static("workload-isolation-type"),
				Str::new_static("firecracker"),
			)]),
		}
	}

	/// The platform properties for an action: operating system, architecture,
	/// the profile's fixed properties and the operator's extras. Later sources
	/// win, so an operator can override a fixed property on purpose. The result
	/// is sorted by name, which REAPI requires for a canonical `Platform`.
	pub fn platform_properties(
		&self,
		platform: Platform,
		extra: &BTreeMap<Str, Str>,
	) -> Vec<(Str, Str)> {
		let mut merged = BTreeMap::new();
		merged.insert(Str::new_static("OSFamily"), os_name(platform.os));
		let (key, value) = self.arch_property(platform.arch);
		merged.insert(key, value);
		merged.extend(self.fixed.iter().map(|(k, v)| (k.clone(), v.clone())));
		merged.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
		merged.into_iter().collect()
	}

	fn arch_property(&self, arch: Arch) -> (Str, Str) {
		match (self.arch, arch) {
			(ArchSpelling::Reapi, Arch::X86_64) => (Str::new_static("ISA"), Str::new_static("x86-64")),
			(ArchSpelling::Reapi, Arch::Aarch64) => (Str::new_static("ISA"), Str::new_static("arm64")),
			(ArchSpelling::Short, Arch::X86_64) => (Str::new_static("Arch"), Str::new_static("amd64")),
			(ArchSpelling::Short, Arch::Aarch64) => {
				(Str::new_static("Arch"), Str::new_static("arm64"))
			},
		}
	}
}

fn os_name(os: Os) -> Str {
	match os {
		Os::Linux => Str::new_static("linux"),
		Os::Macos => Str::new_static("darwin"),
		Os::Windows => Str::new_static("windows"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn props(profile: &Profile, extra: &[(&str, &str)]) -> Vec<(String, String)> {
		let extra = extra
			.iter()
			.map(|(k, v)| (Str::new(k), Str::new(v)))
			.collect();
		profile
			.platform_properties(Platform::LINUX_X86_64, &extra)
			.into_iter()
			.map(|(k, v)| (k.to_string(), v.to_string()))
			.collect()
	}

	#[test]
	fn the_portable_profile_uses_the_standard_vocabulary_and_no_vendor_property() {
		assert_eq!(props(&Profile::reapi(), &[]), [
			("ISA".to_owned(), "x86-64".to_owned()),
			("OSFamily".to_owned(), "linux".to_owned()),
		]);
	}

	#[test]
	fn the_buildbuddy_profile_spells_the_architecture_its_own_way() {
		let properties = props(&Profile::buildbuddy(), &[]);
		assert!(properties.contains(&("Arch".to_owned(), "amd64".to_owned())), "{properties:?}");
		assert!(!properties.iter().any(|(k, _)| k == "ISA"), "{properties:?}");
	}

	#[test]
	fn no_profile_sends_a_runner_recycling_property() {
		for profile in [Profile::reapi(), Profile::buildbuddy()] {
			let properties = props(&profile, &[]);
			assert!(
				!properties
					.iter()
					.any(|(k, _)| k.contains("recycle") || k.contains("preserve")),
				"{properties:?}"
			);
		}
	}

	#[test]
	fn operator_properties_override_fixed_ones_and_the_result_is_sorted() {
		let properties = props(&Profile::buildbuddy(), &[
			("workload-isolation-type", "oci"),
			("container-image", "docker://example/image@sha256:abc"),
		]);
		assert!(properties.contains(&("workload-isolation-type".to_owned(), "oci".to_owned())));
		let mut sorted = properties.clone();
		sorted.sort();
		assert_eq!(properties, sorted);
	}

	#[test]
	fn only_the_buildbuddy_profile_names_a_credential_header() {
		assert_eq!(Profile::reapi().credential_header, None);
		assert_eq!(Profile::buildbuddy().credential_header.as_deref(), Some("x-buildbuddy-api-key"));
	}

	#[test]
	fn a_credential_never_appears_in_debug_output() {
		let credential = Credential::new("super-secret-key-value");
		assert!(!format!("{credential:?}").contains("super-secret"));
		assert!(!format!("{:?}", Some(&credential)).contains("super-secret"));
		assert_eq!(credential.expose(), "super-secret-key-value");
	}

	#[test]
	fn a_profile_holds_no_credential_so_debug_cannot_leak_one() {
		let rendered = format!("{:?}", Profile::buildbuddy());
		assert!(!rendered.to_lowercase().contains("secret"), "{rendered}");
	}
}
