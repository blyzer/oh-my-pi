//! Bounded, read-only `ssh://` resolver.
//!
//! It reads remote files and directories over SFTP, lists configured hosts, and
//! answers `?op=stat`. It never runs a remote command: `read` and `grep` reach
//! it, and command execution belongs to `bash`.

use omp_core::{CowBytes, Str};
use omp_tools::read::{
	Fault,
	resolver::{
		LineOffsetCache, Resolve, ResourceCompletion, ResourceEntry, ResourceList, fuzzy_score,
	},
	selector::ParsedSelector,
};

use crate::ssh::{RemoteMetadata, SshError, SshService};

pub(crate) struct SshResolver {
	service: SshService,
	lines:   LineOffsetCache,
}

impl SshResolver {
	pub(super) fn new(service: SshService) -> Self {
		Self { service, lines: LineOffsetCache::default() }
	}
}

impl Resolve for SshResolver {
	async fn read<'a>(
		&'a self,
		resource: &'a str,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		use super::select_bytes;
		let (alias, path) = parse_resource(resource)?;
		if path == "/" {
			let listing = self.list(resource, 1_000, 1024 * 1024).await?;
			let mut body = String::new();
			for entry in listing.entries {
				body.push_str(if entry.directory { "d " } else { "f " });
				body.push_str(entry.name.as_str());
				body.push('\n');
			}
			return select_bytes(&self.lines, resource, CowBytes::from(body.into_bytes()), selector);
		}
		let bytes = self
			.service
			.read(alias.as_str(), path.as_str(), 8 * 1024 * 1024)
			.await
			.map_err(ssh_fault)?;
		select_bytes(&self.lines, resource, bytes, selector)
	}

	async fn read_query<'a>(
		&'a self,
		resource: &'a str,
		query: Option<&'a str>,
		selector: &'a ParsedSelector,
	) -> Result<CowBytes<'static>, Fault> {
		use super::select_bytes;
		let Some(query) = query else {
			return self.read(resource, selector).await;
		};
		parse_stat_query(query)?;
		let (alias, path) = parse_resource(resource)?;
		let metadata = self.service.stat(&alias, &path).await.map_err(ssh_fault)?;
		select_bytes(&self.lines, resource, stat_report(metadata), selector)
	}

	async fn list(
		&self,
		resource: &str,
		max_entries: usize,
		max_bytes: usize,
	) -> Result<ResourceList, Fault> {
		if resource.is_empty() {
			let aliases = self.service.aliases();
			let truncated = aliases.len() > max_entries;
			let entries = aliases
				.into_iter()
				.take(max_entries)
				.map(|alias| ResourceEntry {
					uri:       Str::new(format!("ssh://{alias}/")),
					name:      alias,
					directory: true,
					size:      0,
				})
				.collect();
			return Ok(ResourceList { entries, truncated });
		}
		let (alias, path) = parse_resource(resource)?;
		let (remote, mut truncated) = self
			.service
			.list(alias.as_str(), path.as_str(), max_entries)
			.await
			.map_err(ssh_fault)?;
		let mut consumed = 0usize;
		let mut entries = Vec::with_capacity(remote.len());
		for entry in remote {
			consumed = consumed.saturating_add(entry.name.len());
			if consumed > max_bytes {
				truncated = true;
				break;
			}
			let child_path = if path == "/" {
				format!("/{}", encode_component(&entry.name))
			} else {
				format!("{}/{}", path.trim_end_matches('/'), encode_component(&entry.name))
			};
			entries.push(ResourceEntry {
				uri:       Str::new(format!("ssh://{alias}{child_path}")),
				name:      entry.name,
				directory: entry.directory,
				size:      entry.size,
			});
		}
		Ok(ResourceList { entries, truncated })
	}

	async fn complete(
		&self,
		query: &str,
		max_results: usize,
	) -> Result<Vec<ResourceCompletion>, Fault> {
		let mut values = self
			.service
			.aliases()
			.into_iter()
			.filter_map(|alias| {
				let score = fuzzy_score(query, &alias)?;
				Some(ResourceCompletion {
					value: Str::new(format!("ssh://{alias}/")),
					description: Str::new_static("configured SSH host"),
					score,
				})
			})
			.collect::<Vec<_>>();
		values.sort_unstable_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.cmp(&b.value)));
		values.truncate(max_results);
		Ok(values)
	}
}

/// Refusal for `?op=exec`: running a remote command is execution, which only
/// `bash` performs.
const REMOTE_EXEC_REFUSED: &str = "ssh:// never runs remote commands; use `bash` with a remote \
                                   SSH command (`ssh <alias> <command>`).";

/// Accepts exactly one `op=stat` query parameter.
///
/// `op=exec` is refused as [`Fault::Unsupported`] naming `bash`, wherever it
/// appears in the query, before any connection is opened.
fn parse_stat_query(query: &str) -> Result<(), Fault> {
	let mut stat = false;
	let mut malformed = false;
	for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
		match (name.as_ref(), value.as_ref()) {
			("op", "exec") => {
				return Err(Fault::Unsupported { message: Str::new_static(REMOTE_EXEC_REFUSED) });
			},
			("op", "stat") if !stat => stat = true,
			_ => malformed = true,
		}
	}
	if malformed || !stat {
		return Err(Fault::Invalid {
			message: Str::new_static("ssh:// accepts only the `op=stat` query."),
		});
	}
	Ok(())
}

/// Renders `?op=stat` metadata as `kind` and `size` lines.
fn stat_report(metadata: RemoteMetadata) -> CowBytes<'static> {
	let kind = if metadata.directory {
		"directory"
	} else {
		"file"
	};
	CowBytes::from(format!("kind: {kind}\nsize: {}\n", metadata.size).into_bytes())
}

pub(crate) fn parse_resource(resource: &str) -> Result<(Str, Str), Fault> {
	let (raw_alias, raw_path) = resource.split_once('/').unwrap_or((resource, ""));
	if raw_alias.is_empty() || raw_alias.contains(['@', ':', '[', ']']) {
		return Err(Fault::Invalid {
			message: Str::new_static(
				"ssh:// authority must be one configured host alias; user, port, and address \
				 overrides are forbidden.",
			),
		});
	}
	let alias = decode_component(raw_alias)?;
	let path = decode_path(raw_path)?;
	Ok((alias, Str::new(format!("/{path}"))))
}

fn decode_path(raw: &str) -> Result<String, Fault> {
	let mut decoded = String::new();
	for segment in raw.split('/') {
		let segment = decode_component(segment)?;
		if segment == "." || segment == ".." || segment.contains(['\\', '\0']) {
			return Err(Fault::Invalid {
				message: Str::new_static(
					"ssh:// paths cannot contain dot segments, backslashes, or NUL bytes.",
				),
			});
		}
		if !decoded.is_empty() {
			decoded.push('/');
		}
		decoded.push_str(&segment);
	}
	Ok(decoded)
}

fn decode_component(raw: &str) -> Result<Str, Fault> {
	let bytes = raw.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' {
			if i + 2 >= bytes.len() {
				return Err(percent_fault());
			}
			let high = hex(bytes[i + 1]).ok_or_else(percent_fault)?;
			let low = hex(bytes[i + 2]).ok_or_else(percent_fault)?;
			out.push(high << 4 | low);
			i += 3;
		} else {
			out.push(bytes[i]);
			i += 1;
		}
	}
	String::from_utf8(out)
		.map(Str::new)
		.map_err(|_| Fault::Invalid {
			message: Str::new_static("ssh:// components must decode to UTF-8."),
		})
}

const fn hex(byte: u8) -> Option<u8> {
	match byte {
		b'0'..=b'9' => Some(byte - b'0'),
		b'a'..=b'f' => Some(byte - b'a' + 10),
		b'A'..=b'F' => Some(byte - b'A' + 10),
		_ => None,
	}
}
fn percent_fault() -> Fault {
	Fault::Invalid { message: Str::new_static("ssh:// contains invalid percent encoding.") }
}

fn encode_component(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for byte in value.bytes() {
		if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
			out.push(char::from(byte));
		} else {
			use std::fmt::Write as _;
			let _ = write!(out, "%{byte:02X}");
		}
	}
	out
}

fn ssh_fault(error: SshError) -> Fault {
	Fault::Source { message: Str::new(error.to_string()) }
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ssh::HostStore;

	/// A resolver with no configured hosts: any operation that reaches the SSH
	/// service fails with `SshError::UnknownHost` before opening a connection.
	fn resolver() -> SshResolver {
		SshResolver::new(SshService::new(HostStore::default()))
	}

	#[tokio::test]
	async fn exec_query_is_refused_naming_bash_before_any_connection() {
		let resolver = resolver();
		for (resource, query) in [
			("prod/etc/hosts", "op=exec&command=uname%20-a"),
			("prod/etc/hosts", "command=id&op=exec"),
			("prod/etc/hosts", "op=ex%65c&command=id"),
			("prod/etc/hosts", "op=stat&op=exec"),
			("prod/", "op=exec"),
			("", "op=exec&command=id"),
		] {
			let fault = resolver
				.read_query(resource, Some(query), &ParsedSelector::None)
				.await
				.expect_err(query);
			let Fault::Unsupported { message } = &fault else {
				panic!("{resource}?{query} must be refused as unsupported, got {fault:?}");
			};
			assert!(message.contains("`bash`"), "{resource}?{query}: {message}");
			assert!(message.contains("never runs remote commands"), "{resource}?{query}: {message}");
		}
	}

	#[tokio::test]
	async fn stat_query_and_plain_reads_still_reach_the_host_authority() {
		let resolver = resolver();
		for query in [Some("op=stat"), Some("op=st%61t"), None] {
			let fault = resolver
				.read_query("prod/etc/hosts", query, &ParsedSelector::None)
				.await
				.expect_err("prod is not configured");
			assert!(
				matches!(&fault, Fault::Source { message } if message == "SSH host prod is not configured"),
				"{query:?}: {fault:?}"
			);
		}
	}

	#[tokio::test]
	async fn malformed_queries_are_invalid() {
		let resolver = resolver();
		for query in ["", "op=list", "op=stat&op=stat", "op=stat&command=id", "command=id", "op="] {
			let fault = resolver
				.read_query("prod/etc/hosts", Some(query), &ParsedSelector::None)
				.await
				.expect_err(query);
			assert!(
				matches!(&fault, Fault::Invalid { message } if message == "ssh:// accepts only the `op=stat` query."),
				"{query:?}: {fault:?}"
			);
		}
	}

	#[test]
	fn stat_report_emits_kind_and_size_lines() {
		assert_eq!(
			stat_report(RemoteMetadata { directory: false, size: 42 }).as_ref(),
			b"kind: file\nsize: 42\n"
		);
		assert_eq!(
			stat_report(RemoteMetadata { directory: true, size: 0 }).as_ref(),
			b"kind: directory\nsize: 0\n"
		);
	}
}
