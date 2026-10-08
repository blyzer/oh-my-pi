//! The hosts a fetch reaches, named by the environment resolvers that perform
//! it, and the approval subjects keyed on them.
//!
//! A fetch is approved per host, never per tool: under `always-ask` a session
//! grant for one host covers that host alone. The host comes from the resolver
//! that will carry the request, not from the URL text alone: `issue://5` is
//! the GitHub host of the workspace's git remote, and an `mcp://` resource is
//! the server advertising it. An http(s) URL's host is the authored one, the
//! host its arguments name; the redirects the client follows and the site
//! readers that re-target a fetch to a mirror or an API host are known only
//! once it runs, and the authored host's grant covers them.

use omp_core::{Str, sf};
use omp_proto::policy::v1 as pb;
use omp_tools::read::{resolver::ResolverTable, selector, web};
use thiserror::Error;

use crate::tool_url::UrlResolver;

/// The approval kind of a fetch requirement, Python's `ApprovalKind.NETWORK`.
pub const NETWORK_APPROVAL_KIND: &str = "network";

/// The environment resolver that performs a fetch, and so names its host.
#[derive(
	Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, strum::Display, strum::IntoStaticStr,
)]
#[strum(serialize_all = "lowercase")]
pub enum FetchResolver {
	/// An http(s) URL, by the host and port its arguments name.
	Http,
	/// `issue://` and `pr://`, through the GitHub API with the user's stored
	/// credentials.
	Github,
	/// A configured `ssh://` host alias, with its configured credentials.
	Ssh,
	/// A mounted MCP server, with its configured credentials.
	Mcp,
}

/// One host a fetch reaches, as the resolver that performs it names it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FetchHost {
	resolver: FetchResolver,
	host:     Str,
	/// Present exactly for [`FetchResolver::Http`].
	port:     Option<u16>,
}

const _: () = assert!(size_of::<FetchHost>() <= 32, "FetchHost must stay compact");

impl FetchHost {
	/// The authored host and port of an http(s) URL.
	#[must_use]
	pub fn http(host: impl Into<Str>, port: u16) -> Self {
		Self { resolver: FetchResolver::Http, host: host.into(), port: Some(port) }
	}

	/// A GitHub host reached through its API.
	#[must_use]
	pub fn github(host: impl Into<Str>) -> Self {
		Self { resolver: FetchResolver::Github, host: host.into(), port: None }
	}

	/// A configured SSH host alias.
	#[must_use]
	pub fn ssh(alias: impl Into<Str>) -> Self {
		Self { resolver: FetchResolver::Ssh, host: alias.into(), port: None }
	}

	/// A mounted MCP server.
	#[must_use]
	pub fn mcp(server: impl Into<Str>) -> Self {
		Self { resolver: FetchResolver::Mcp, host: server.into(), port: None }
	}

	/// The resolver that performs the fetch.
	#[must_use]
	pub const fn resolver(&self) -> FetchResolver {
		self.resolver
	}

	/// Host name, SSH alias, or MCP server name.
	#[must_use]
	pub fn host(&self) -> &str {
		&self.host
	}

	/// The port of an http(s) host.
	#[must_use]
	pub const fn port(&self) -> Option<u16> {
		self.port
	}

	/// The approval subject a session grant for this host is keyed on:
	/// `<resolver>:<host>`, and `:<port>` for an http(s) host.
	#[must_use]
	pub fn subject(&self) -> Str {
		let resolver = self.resolver;
		let host = &self.host;
		match self.port {
			Some(port) => sf!("{resolver}:{host}:{port}"),
			None => sf!("{resolver}:{host}"),
		}
	}

	/// Prompt prose naming this host and the credentials its fetch presents.
	#[must_use]
	pub fn describe(&self) -> Str {
		let host = &self.host;
		match (self.resolver, self.port) {
			(FetchResolver::Http, Some(port)) => sf!(
				"the authored host {host}:{port} (anonymously; redirects and site readers that \
				 re-target this fetch are covered by its approval)"
			),
			(FetchResolver::Http, None) => sf!("the authored host {host}"),
			(FetchResolver::Github, _) => {
				sf!("the GitHub host {host} through its API, with your stored GitHub credentials")
			},
			(FetchResolver::Ssh, _) => {
				sf!("the SSH host `{host}`, with its configured credentials")
			},
			(FetchResolver::Mcp, _) => {
				sf!("the MCP server `{host}`, with its configured credentials")
			},
		}
	}
}

/// The approval subject of a fetch by `tool` whose hosts the environment
/// cannot name: the tool's own, `tool:<name>`.
///
/// It never equals a host's subject, so a grant for it covers no named host
/// and no host's grant covers it.
#[must_use]
pub fn tool_fetch_subject(tool: &str) -> Str {
	sf!("tool:{tool}")
}

/// The `(subject, description)` of each requirement a fetch by `tool` raises:
/// one per host in `hosts`, which must be distinct, or the tool's own when the
/// environment named none.
pub fn fetch_subjects<'a>(
	tool: &'a str,
	hosts: &'a [FetchHost],
) -> impl Iterator<Item = (Str, Str)> + 'a {
	let fallback = hosts.is_empty().then(|| {
		(
			tool_fetch_subject(tool),
			sf!("the hosts `{tool}` reaches, which the environment cannot name before it runs"),
		)
	});
	hosts
		.iter()
		.map(|host| (host.subject(), host.describe()))
		.chain(fallback)
}

/// A wire [`pb::FetchTarget`] that does not name exactly one host.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FetchTargetError {
	/// The resolver is unspecified or unknown to this schema.
	#[error("fetch target names no known resolver ({value})")]
	UnknownResolver {
		/// Raw enum value on the wire.
		value: i32,
	},
	/// The host is empty.
	#[error("fetch target names no host")]
	EmptyHost,
	/// An http(s) target carries no port.
	#[error("http fetch target carries no port")]
	MissingPort,
	/// The port does not fit a TCP port.
	#[error("fetch target port {port} is out of range")]
	PortOutOfRange {
		/// Port on the wire.
		port: u32,
	},
	/// A target other than http(s) carries a port.
	#[error("non-http fetch target carries port {port}")]
	UnexpectedPort {
		/// Port on the wire.
		port: u32,
	},
}

impl From<&FetchHost> for pb::FetchTarget {
	fn from(host: &FetchHost) -> Self {
		let resolver = match host.resolver {
			FetchResolver::Http => pb::FetchResolver::Http,
			FetchResolver::Github => pb::FetchResolver::Github,
			FetchResolver::Ssh => pb::FetchResolver::Ssh,
			FetchResolver::Mcp => pb::FetchResolver::Mcp,
		};
		Self {
			resolver: resolver as i32,
			host:     host.host.to_string(),
			port:     host.port.map(u32::from),
			props:    None,
		}
	}
}

impl TryFrom<&pb::FetchTarget> for FetchHost {
	type Error = FetchTargetError;

	fn try_from(target: &pb::FetchTarget) -> Result<Self, Self::Error> {
		let resolver = match pb::FetchResolver::try_from(target.resolver) {
			Ok(pb::FetchResolver::Http) => FetchResolver::Http,
			Ok(pb::FetchResolver::Github) => FetchResolver::Github,
			Ok(pb::FetchResolver::Ssh) => FetchResolver::Ssh,
			Ok(pb::FetchResolver::Mcp) => FetchResolver::Mcp,
			Ok(pb::FetchResolver::Unspecified) | Err(_) => {
				return Err(FetchTargetError::UnknownResolver { value: target.resolver });
			},
		};
		if target.host.is_empty() {
			return Err(FetchTargetError::EmptyHost);
		}
		let port = match (resolver, target.port) {
			(FetchResolver::Http, Some(port)) => {
				Some(u16::try_from(port).map_err(|_| FetchTargetError::PortOutOfRange { port })?)
			},
			(FetchResolver::Http, None) => return Err(FetchTargetError::MissingPort),
			(_, Some(port)) => return Err(FetchTargetError::UnexpectedPort { port }),
			(_, None) => None,
		};
		Ok(Self { resolver, host: Str::from(target.host.as_str()), port })
	}
}

/// Names every distinct host the fetch `locators` reach, sorted.
///
/// `None` when any locator is not a fetch the environment can name (an
/// unparsable URL, `issue://5` in a workspace without a GitHub remote, an
/// `mcp://` resource no mounted server advertises, a scheme no fetching
/// resolver serves). The resolver performing such a fetch fails the same way,
/// but the call is still approved as the tool's own rather than on the hosts
/// that could be named.
///
/// Naming reads no network; an `issue://` or `pr://` locator without a host
/// reads the workspace's git config, as the resolver does when it runs.
pub(crate) fn resolve_locators(
	resources: &ResolverTable<UrlResolver>,
	locators: &[Str],
) -> Option<Vec<FetchHost>> {
	let mut hosts = locators
		.iter()
		.map(|locator| resolve_locator(resources, locator))
		.collect::<Option<Vec<_>>>()?;
	hosts.sort_unstable();
	hosts.dedup();
	Some(hosts)
}

/// Names the host one fetch locator reaches: an http(s) URL as `read`
/// recognizes it, else an internal URL by the resolver its scheme routes to.
fn resolve_locator(resources: &ResolverTable<UrlResolver>, locator: &str) -> Option<FetchHost> {
	if let Some(target) = web::parse_target(locator).ok()? {
		let host = target.url.host_str()?;
		let port = target.url.port_or_known_default()?;
		return Some(FetchHost::http(host, port));
	}
	let uri = selector::parse_uri(locator).ok()??;
	resources
		.get(uri.scheme)?
		.fetch_host(uri.resource, uri.query)
}

#[cfg(test)]
mod tests {
	use omp_proto::policy::v1 as pb;

	use super::{FetchHost, FetchResolver, FetchTargetError, fetch_subjects, tool_fetch_subject};

	/// Each resolver keys its own subject; a port distinguishes http(s)
	/// endpoints and nothing else carries one; the tool fallback never
	/// collides with a host.
	#[test]
	fn subjects_are_keyed_by_resolver_and_host() {
		for (host, subject) in [
			(FetchHost::http("docs.rs", 443), "http:docs.rs:443"),
			(FetchHost::http("docs.rs", 80), "http:docs.rs:80"),
			(FetchHost::github("ghe.example.com"), "github:ghe.example.com"),
			(FetchHost::ssh("prod"), "ssh:prod"),
			(FetchHost::mcp("linear"), "mcp:linear"),
		] {
			assert_eq!(host.subject(), subject);
		}
		assert_eq!(tool_fetch_subject("read"), "tool:read");
		assert!(
			FetchHost::http("docs.rs", 443)
				.describe()
				.contains("authored host docs.rs:443")
		);
		let named = [FetchHost::http("a.example", 443), FetchHost::ssh("prod")];
		assert_eq!(
			fetch_subjects("read", &named)
				.map(|(subject, _)| subject)
				.collect::<Vec<_>>(),
			["http:a.example:443", "ssh:prod"]
		);
		assert_eq!(
			fetch_subjects("read", &[])
				.map(|(subject, _)| subject)
				.collect::<Vec<_>>(),
			["tool:read"]
		);
	}

	/// The wire form round-trips, and a target that does not name exactly one
	/// host is refused with a typed error.
	#[test]
	fn fetch_targets_survive_the_wire_and_refuse_malformed_hosts() {
		for host in [
			FetchHost::http("docs.rs", 8443),
			FetchHost::github("github.com"),
			FetchHost::ssh("prod"),
			FetchHost::mcp("linear"),
		] {
			let wire = pb::FetchTarget::from(&host);
			assert_eq!(FetchHost::try_from(&wire), Ok(host));
		}
		let target = |resolver: pb::FetchResolver, host: &str, port: Option<u32>| pb::FetchTarget {
			resolver: resolver as i32,
			host: host.to_owned(),
			port,
			props: None,
		};
		for (wire, error) in [
			(target(pb::FetchResolver::Unspecified, "x", None), FetchTargetError::UnknownResolver {
				value: 0,
			}),
			(target(pb::FetchResolver::Ssh, "", None), FetchTargetError::EmptyHost),
			(target(pb::FetchResolver::Http, "x", None), FetchTargetError::MissingPort),
			(target(pb::FetchResolver::Http, "x", Some(70_000)), FetchTargetError::PortOutOfRange {
				port: 70_000,
			}),
			(target(pb::FetchResolver::Mcp, "x", Some(1)), FetchTargetError::UnexpectedPort {
				port: 1,
			}),
		] {
			assert_eq!(FetchHost::try_from(&wire), Err(error));
		}
		assert_eq!(
			FetchHost::try_from(&pb::FetchTarget { resolver: 99, ..pb::FetchTarget::default() }),
			Err(FetchTargetError::UnknownResolver { value: 99 })
		);
		assert_eq!(FetchHost::github("github.com").resolver(), FetchResolver::Github);
	}
}
