//! The remote endpoint an operator configures, validated.
//!
//! The credential travels in a request header, so a plaintext connection to a
//! remote host would send it in the clear. The only plaintext endpoint this
//! crate accepts is one on a loopback address, which the fake-server tests and
//! a local development server use (ADR 0040, Part F).

use std::net::IpAddr;

use omp_core::Str;

/// Whether the connection is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
	/// `grpcs://`: TLS.
	Tls,
	/// `grpc://`: plaintext, loopback only.
	Plaintext,
}

/// A validated `grpcs://host[:port]` or loopback `grpc://host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
	scheme: Scheme,
	host:   Str,
	port:   u16,
}

/// Why an endpoint was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
	/// The scheme is not `grpc` or `grpcs`.
	#[error("endpoint scheme must be grpcs:// (or grpc:// to a loopback address)")]
	Scheme,
	/// There is no host.
	#[error("endpoint has no host")]
	NoHost,
	/// The port is not a number in `1..=65535`.
	#[error("endpoint port is not a valid port number")]
	Port,
	/// The rest of the URL (a path, query, fragment or credentials) is not
	/// allowed: the instance name is a separate setting.
	#[error("endpoint must be scheme://host[:port] with nothing after it")]
	Trailing,
	/// A plaintext connection to a host that is not a loopback address.
	#[error("plaintext grpc:// is only allowed to a loopback address; use grpcs://")]
	PlaintextToRemote,
}

impl Endpoint {
	/// Parses and validates an endpoint.
	///
	/// # Errors
	///
	/// [`EndpointError`] for any malformed or unsafe endpoint.
	pub fn parse(text: &str) -> Result<Self, EndpointError> {
		let (scheme, rest) = if let Some(rest) = text.strip_prefix("grpcs://") {
			(Scheme::Tls, rest)
		} else if let Some(rest) = text.strip_prefix("grpc://") {
			(Scheme::Plaintext, rest)
		} else {
			return Err(EndpointError::Scheme);
		};
		if rest.is_empty() {
			return Err(EndpointError::NoHost);
		}
		if rest.contains(['/', '?', '#', '@']) {
			return Err(EndpointError::Trailing);
		}
		let (host, port) = split_host_port(rest)?;
		if host.is_empty() {
			return Err(EndpointError::NoHost);
		}
		if scheme == Scheme::Plaintext && !is_loopback(host) {
			return Err(EndpointError::PlaintextToRemote);
		}
		let port = port.unwrap_or(match scheme {
			Scheme::Tls => 443,
			Scheme::Plaintext => 80,
		});
		Ok(Self { scheme, host: Str::new(host), port })
	}

	/// Whether the connection is encrypted.
	pub const fn scheme(&self) -> Scheme {
		self.scheme
	}

	/// The host, without brackets for an IPv6 literal.
	pub fn host(&self) -> &str {
		&self.host
	}

	/// The port: 443 for `grpcs://` and 80 for `grpc://` when none was given.
	pub const fn port(&self) -> u16 {
		self.port
	}

	/// The URL a transport connects to: `https://` or `http://`.
	pub fn transport_url(&self) -> String {
		let scheme = match self.scheme {
			Scheme::Tls => "https",
			Scheme::Plaintext => "http",
		};
		if self.host.contains(':') {
			format!("{scheme}://[{}]:{}", self.host, self.port)
		} else {
			format!("{scheme}://{}:{}", self.host, self.port)
		}
	}
}

fn split_host_port(rest: &str) -> Result<(&str, Option<u16>), EndpointError> {
	if let Some(bracketed) = rest.strip_prefix('[') {
		let (host, after) = bracketed.split_once(']').ok_or(EndpointError::NoHost)?;
		return match after.strip_prefix(':') {
			Some(port) => Ok((host, Some(parse_port(port)?))),
			None if after.is_empty() => Ok((host, None)),
			None => Err(EndpointError::Trailing),
		};
	}
	match rest.rsplit_once(':') {
		Some((host, port)) if !host.contains(':') => Ok((host, Some(parse_port(port)?))),
		Some(_) => Err(EndpointError::Trailing),
		None => Ok((rest, None)),
	}
}

fn parse_port(port: &str) -> Result<u16, EndpointError> {
	match port.parse::<u16>() {
		Ok(0) | Err(_) => Err(EndpointError::Port),
		Ok(port) => Ok(port),
	}
}

fn is_loopback(host: &str) -> bool {
	host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_tls_endpoint_defaults_to_443() {
		let endpoint = Endpoint::parse("grpcs://remote.example.com").expect("parses");
		assert_eq!(endpoint.scheme(), Scheme::Tls);
		assert_eq!(endpoint.port(), 443);
		assert_eq!(endpoint.transport_url(), "https://remote.example.com:443");
	}

	#[test]
	fn an_explicit_port_is_kept() {
		let endpoint = Endpoint::parse("grpcs://remote.example.com:8443").expect("parses");
		assert_eq!(endpoint.port(), 8443);
	}

	#[test]
	fn plaintext_is_allowed_only_to_loopback() {
		for ok in ["grpc://127.0.0.1:9000", "grpc://localhost:9000", "grpc://[::1]:9000"] {
			let endpoint = Endpoint::parse(ok).unwrap_or_else(|error| panic!("{ok}: {error}"));
			assert_eq!(endpoint.scheme(), Scheme::Plaintext, "{ok}");
		}
		for refused in [
			"grpc://remote.example.com",
			"grpc://10.0.0.5:9000",
			"grpc://192.168.1.2",
			"grpc://localhost.evil.example",
		] {
			assert_eq!(Endpoint::parse(refused), Err(EndpointError::PlaintextToRemote), "{refused}");
		}
	}

	#[test]
	fn an_ipv6_literal_round_trips_into_a_transport_url() {
		let endpoint = Endpoint::parse("grpc://[::1]:9000").expect("parses");
		assert_eq!(endpoint.host(), "::1");
		assert_eq!(endpoint.transport_url(), "http://[::1]:9000");
	}

	#[test]
	fn other_schemes_and_empty_hosts_are_refused() {
		assert_eq!(Endpoint::parse("https://example.com"), Err(EndpointError::Scheme));
		assert_eq!(Endpoint::parse("example.com:443"), Err(EndpointError::Scheme));
		assert_eq!(Endpoint::parse("grpcs://"), Err(EndpointError::NoHost));
		assert_eq!(Endpoint::parse("grpcs://:443"), Err(EndpointError::NoHost));
	}

	#[test]
	fn paths_credentials_and_bad_ports_are_refused() {
		assert_eq!(Endpoint::parse("grpcs://example.com/path"), Err(EndpointError::Trailing));
		assert_eq!(Endpoint::parse("grpcs://user@example.com"), Err(EndpointError::Trailing));
		assert_eq!(Endpoint::parse("grpcs://example.com?x=1"), Err(EndpointError::Trailing));
		assert_eq!(Endpoint::parse("grpcs://example.com:0"), Err(EndpointError::Port));
		assert_eq!(Endpoint::parse("grpcs://example.com:99999"), Err(EndpointError::Port));
		assert_eq!(Endpoint::parse("grpcs://example.com:http"), Err(EndpointError::Port));
	}
}
