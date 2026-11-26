//! `omp ext grant <id> --credential <provider>`: the operator's credential
//! placement grant (ADR 0075).
//!
//! A grant names one provider, the https origins its stored credential may
//! reach and the operations that may carry it. It is written to the same
//! protected store as the CONTROL grants, bound to the extension's identity and
//! manifest digest, and checked again under the store lock so an extension that
//! changed after the operator confirmed it is never granted. The extension
//! never receives the key: the host adds it at egress.

use std::path::{Path, PathBuf};

use miette::miette;
use omp_ext::{
	Layer,
	credential_grant::{CredentialGrant, CredentialOperation, CredentialOrigin},
	trust::GrantsFile,
};

use super::{ExtGrantArgs, GrantRefusal, StatePaths, create_private_store, grants_update_failure};

/// A port is decimal digits only: `str::parse` would also take a leading `+`.
fn parse_port(text: &str) -> Option<u16> {
	(!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
		.then(|| text.parse().ok())
		.flatten()
}

/// Parses `https://host` or `https://host:port`.
pub(super) fn parse_origin(text: &str) -> miette::Result<CredentialOrigin> {
	// A rejected origin is echoed back, so never echo userinfo the operator typed.
	let shown = if text.contains('@') {
		"<an origin with userinfo>"
	} else {
		text
	};
	let invalid = || {
		miette!("`{shown}` is not an origin: write it as https://host or https://host:port, no path")
	};
	let (scheme, rest) = text.split_once("://").ok_or_else(invalid)?;
	if rest.is_empty() || rest.contains(['/', '?', '#', '@']) {
		return Err(invalid());
	}
	let (host, port) = if rest.starts_with('[') {
		let end = rest.find(']').ok_or_else(invalid)?;
		let (host, tail) = rest.split_at(end + 1);
		match tail {
			"" => (host, None),
			tail => {
				let port = tail
					.strip_prefix(':')
					.and_then(parse_port)
					.ok_or_else(invalid)?;
				(host, Some(port))
			},
		}
	} else {
		match rest.rsplit_once(':') {
			Some((host, port)) => (host, Some(parse_port(port).ok_or_else(invalid)?)),
			None => (rest, None),
		}
	};
	CredentialOrigin::new(scheme, host, port).map_err(|error| miette!("origin `{text}`: {error}"))
}

/// Parses `METHOD /path/prefix`.
pub(super) fn parse_operation(text: &str) -> miette::Result<CredentialOperation> {
	let (method, prefix) = text.trim().split_once(' ').ok_or_else(|| {
		miette!("`{text}` is not an operation: write it as \"POST /api/v1/\" (method, space, path)")
	})?;
	CredentialOperation::new(method.trim(), prefix.trim())
		.map_err(|error| miette!("operation `{text}`: {error}"))
}

/// What `--revoke --credential` reports. The revoke is scoped by extension id
/// and provider only, so it also clears a row left by an earlier install at
/// another root; withdrawing authority never widens it. A session already
/// running keeps the grant it was admitted with until it restarts.
pub(super) fn revoke_report(args: &ExtGrantArgs, provider: &str, removed: usize) -> String {
	if removed == 0 {
		return format!(
			"{}: no credential grant for `{provider}` was recorded; nothing changed",
			args.id
		);
	}
	format!(
		"{}: removed {removed} credential grant(s) for `{provider}`\nrunning sessions keep this \
		 grant until they restart",
		args.id
	)
}

/// Grants or revokes the credential placement grant `args` names. `consent` is
/// asked once, with the full scope, before anything is written.
pub(super) fn grant_with(
	state: &StatePaths,
	data_dir: &Path,
	home: &Path,
	writable_roots: &[PathBuf],
	args: &ExtGrantArgs,
	consent: impl FnOnce(&str) -> miette::Result<bool>,
) -> miette::Result<()> {
	let provider = args
		.credential
		.as_ref()
		.ok_or_else(|| miette!("a credential grant names its provider with --credential"))?;
	let extension_root = omp_driver::discovery::native::user_extension_root(home)
		.map_err(|error| miette!("{error}"))?;
	// Grant and revoke both rewrite grants.toml: refuse either on an unsafe store.
	omp_driver::discovery::native::check_grant_locations(
		data_dir,
		&extension_root,
		&state.project,
		writable_roots,
	)
	.map_err(|error| miette!("refusing to change credential grants for `{}`: {error}", args.id))?;
	if args.revoke {
		let removed =
			GrantsFile::revoke_credential_grants(&state.grants, &args.id, Some(provider.as_str()))
				.map_err(grants_update_failure)?;
		println!("{}", revoke_report(args, provider.as_str(), removed));
		return Ok(());
	}
	omp_driver::discovery::native::check_grant_directory_modes(data_dir, &extension_root)
		.map_err(|error| miette::Report::new(GrantRefusal::from(error)))?;
	if args.origins.is_empty() || args.operations.is_empty() {
		return Err(miette!(
			"a credential grant names where the key may go: pass at least one --origin \
			 (https://host[:port]) and one --operation (\"METHOD /path\")"
		));
	}
	let origins = args
		.origins
		.iter()
		.map(|origin| parse_origin(origin.as_str()))
		.collect::<miette::Result<Vec<_>>>()?;
	let operations = args
		.operations
		.iter()
		.map(|operation| parse_operation(operation.as_str()))
		.collect::<miette::Result<Vec<_>>>()?;
	let identity =
		omp_driver::discovery::native::user_extension_grant_identity(home, args.id.as_str())
			.map_err(|error| miette!("{error}"))?
			.ok_or_else(|| {
				miette!(
					"credential grants are user-layer only: `{}` is not installed under {}",
					args.id,
					extension_root.display()
				)
			})?;
	let mut record = CredentialGrant {
		id: identity.id.clone(),
		publisher: identity.publisher.clone(),
		layer: Layer::Client,
		capability_digest: identity.capability_digest.clone(),
		provider: provider.clone(),
		origins,
		operations,
		granted_at: omp_core::Str::new_static(""),
		granted_by: omp_core::Str::new_static("explicit-grant"),
	};
	record
		.validate()
		.map_err(|error| miette!("refusing a credential grant for `{}`: {error}", args.id))?;
	let scope = record
		.operations
		.iter()
		.flat_map(|operation| {
			record.origins.iter().map(move |origin| {
				format!(
					"  {} https://{}:{}{}",
					operation.method, origin.host, origin.port, operation.path_prefix
				)
			})
		})
		.collect::<Vec<_>>()
		.join("\n");
	if !consent(&format!(
		"Let `{}` at {} send your stored `{provider}` credential on:\n{scope}\nThe extension never \
		 sees the key; the host adds it when the request leaves.",
		args.id,
		identity.root.display()
	))? {
		return Err(miette!(
			"credential grant for `{}` needs consent: pass --yes or answer the prompt in a terminal",
			args.id
		));
	}
	// Stamp the grant when the operator consented, not before the prompt.
	record.granted_at = omp_core::Str::new(jiff::Timestamp::now().to_string());
	if let Some(parent) = state.grants.parent() {
		create_private_store(parent).map_err(|error| miette!("{error}"))?;
	}
	// The checks above ran before the prompt and before the lock. Run them again
	// under the lock, and write only if the store and the extension still match
	// what the operator confirmed.
	GrantsFile::try_update(&state.grants, |grants| {
		omp_driver::discovery::native::check_grant_locations(
			data_dir,
			&extension_root,
			&state.project,
			writable_roots,
		)
		.map_err(GrantRefusal::from)?;
		omp_driver::discovery::native::check_grant_directory_modes(data_dir, &extension_root)
			.map_err(GrantRefusal::from)?;
		let current =
			omp_driver::discovery::native::user_extension_grant_identity(home, args.id.as_str())?
				.ok_or_else(|| GrantRefusal::Gone { id: args.id.clone() })?;
		if current.root != identity.root {
			return Err(GrantRefusal::Moved {
				id:  args.id.clone(),
				was: identity.root.clone(),
				now: current.root,
			});
		}
		if current != identity {
			return Err(GrantRefusal::Changed { id: args.id.clone(), root: identity.root.clone() });
		}
		grants.replace_credential_grant(record);
		Ok::<(), GrantRefusal>(())
	})
	.map_err(grants_update_failure)?
	.map_err(miette::Report::new)?;
	println!("{}: granted `{provider}` placement", args.id);
	Ok(())
}
