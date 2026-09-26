//! v1 `ssh.json` hosts converted into a v2 `hosts.toml`.
//!
//! v1 shape (`packages/coding-agent/src/discovery/ssh.ts`):
//! `{"hosts": {<alias>: {host, username?, port?, keyPath?|key?, description?,
//! compat?}}}`, where `port` and `compat` may be strings and every value went
//! through `${VAR}` expansion. v1 connected with the system `ssh`, which checks
//! `~/.ssh/known_hosts`; v2 pins each host's `SHA256:` key, so the pin is
//! read from the same `known_hosts` entry v1's `ssh` trusted. A name v2
//! already declares keeps v2's host.

use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use omp_core::Str;
use omp_envd::ssh::{
	AuthPolicy, HostConfig, HostStore, known_host_fingerprint, validate_configured_host,
};
use serde::Deserialize;

use super::{AssetError, Entries};
use crate::v1_import::{
	ImportMode, ImportOutcome, V1Item,
	report::{Attention, NotMigratable, SkipReason},
};

/// A v1 `ssh.json` document.
#[derive(Debug, Deserialize)]
struct V1SshFile {
	#[serde(default)]
	hosts: BTreeMap<Str, V1Host>,
}

/// One v1 host declaration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct V1Host {
	host:        Option<Str>,
	username:    Option<Str>,
	port:        Option<V1Port>,
	compat:      Option<V1Flag>,
	key_path:    Option<Str>,
	key:         Option<Str>,
	description: Option<Str>,
}

/// v1 accepted a number or a numeric string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum V1Port {
	Number(f64),
	Text(Str),
}

impl V1Port {
	fn port(&self) -> Option<u16> {
		match self {
			Self::Number(number) => {
				let valid = number.fract() == 0.0 && (1.0..=f64::from(u16::MAX)).contains(number);
				#[expect(
					clippy::cast_possible_truncation,
					clippy::cast_sign_loss,
					reason = "checked to be a whole number in the u16 range"
				)]
				valid.then_some(*number as u16)
			},
			Self::Text(text) => text.trim().parse().ok().filter(|port| *port != 0),
		}
	}
}

/// v1 accepted a boolean or `true`/`1`/`yes` (and their negations).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum V1Flag {
	Bool(bool),
	Text(Str),
}

impl V1Flag {
	fn set(&self) -> bool {
		match self {
			Self::Bool(value) => *value,
			Self::Text(text) => ["true", "1", "yes"]
				.iter()
				.any(|set| text.trim().eq_ignore_ascii_case(set)),
		}
	}
}

impl V1Host {
	fn strings(&self) -> impl Iterator<Item = &str> {
		[&self.host, &self.username, &self.key_path, &self.key]
			.into_iter()
			.flatten()
			.map(Str::as_str)
			.chain(match &self.port {
				Some(V1Port::Text(text)) => Some(text.as_str()),
				_ => None,
			})
	}

	/// The v2 host, pinned from `known_hosts`.
	fn convert(&self, alias: &str, home: &Path) -> Result<HostConfig, AssetError> {
		if self.strings().any(|value| value.contains("${")) {
			return Err(AssetError::SshEnvironment);
		}
		let address = self
			.host
			.clone()
			.filter(|host| !host.trim().is_empty())
			.ok_or(AssetError::SshNoAddress)?;
		let user = self
			.username
			.clone()
			.filter(|user| !user.trim().is_empty())
			.ok_or(AssetError::SshNoUser)?;
		let port = match &self.port {
			None => 22,
			Some(port) => port.port().ok_or(AssetError::SshPort)?,
		};
		let auth = match self.key_path.as_ref().or(self.key.as_ref()) {
			None => AuthPolicy::Agent,
			Some(path) => AuthPolicy::Key { path: expand_tilde(path, home) },
		};
		let known_hosts = home.join(".ssh").join("known_hosts");
		let host_key = known_host_fingerprint(&known_hosts, &address, port)
			.map_err(AssetError::SshKnownHosts)?
			.ok_or(AssetError::SshNoHostKey)?;
		let mut host = HostConfig::new(address, user, host_key, auth);
		host.port = port;
		validate_configured_host(alias, &host).map_err(AssetError::SshHost)?;
		Ok(host)
	}
}

/// `~` and `~/…` against `home`, as v1's `expandTilde` did.
fn expand_tilde(path: &str, home: &Path) -> PathBuf {
	match path.strip_prefix('~') {
		Some("") => home.to_owned(),
		Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
		_ => PathBuf::from(path),
	}
}

/// Converts the v1 hosts in `source` into `destination`.
pub(super) fn convert(
	out: &mut Entries,
	item: V1Item,
	source: Option<PathBuf>,
	root: &Path,
	destination: &Path,
	home: &Path,
) -> Result<(), AssetError> {
	let Some(source) = source else {
		out.push(item, None, None, ImportOutcome::NothingToImport);
		return Ok(());
	};
	let bytes = fs::read(&source)
		.map_err(|error| AssetError::Read { path: source.clone(), source: error })?;
	let file = match serde_json::from_slice::<V1SshFile>(&bytes) {
		Ok(file) => file,
		Err(error) => {
			out.push(
				item,
				Some(source),
				Some(super::relative(root, destination)),
				ImportOutcome::NeedsAttention(Attention::Incompatible(
					AssetError::SshDocument(error).into(),
				)),
			);
			return Ok(());
		},
	};
	if file.hosts.is_empty() {
		out.push(item, Some(source), None, ImportOutcome::NothingToImport);
		return Ok(());
	}
	let store = HostStore::load(destination).map_err(AssetError::SshStore)?;
	let existing = store.aliases();
	for (alias, v1) in file.hosts {
		for (field, lost) in [
			(
				"description",
				v1.description
					.as_ref()
					.is_some_and(|text| !text.trim().is_empty()),
			),
			("compat", v1.compat.as_ref().is_some_and(V1Flag::set)),
		] {
			if lost {
				out.push(
					item,
					Some(source.clone()),
					Some(Str::from(format_args!("{alias} {field}"))),
					ImportOutcome::NotMigratable(NotMigratable::NoV2Equivalent),
				);
			}
		}
		let outcome = match v1.convert(&alias, home) {
			Err(error) => ImportOutcome::NeedsAttention(Attention::Incompatible(error.into())),
			Ok(host) if existing.contains(&alias) => {
				if store.get(&alias).map_err(AssetError::SshStore)? == host {
					ImportOutcome::Skipped(SkipReason::AlreadyPresent)
				} else {
					ImportOutcome::NeedsAttention(Attention::Conflict)
				}
			},
			Ok(host) => {
				if out.mode == ImportMode::Apply {
					store
						.upsert(destination, alias.clone(), host)
						.map_err(AssetError::SshStore)?;
				}
				out.copied()
			},
		};
		out.push(item, Some(source.clone()), Some(alias), outcome);
	}
	Ok(())
}
