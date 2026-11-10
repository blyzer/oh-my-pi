//! Details-URL classification.
//!
//! [`route`] reads an absolute `http` or `https` URL into a provider host
//! and a path. It does not contact the host and does not match the host
//! against a product list. A port is kept only when the URL named one.
//! Userinfo is rejected so a credential cannot ride inside a route.
//!
//! Empty path segments are skipped. `.` and `..` are rejected rather than
//! resolved. The stored path is percent-decoded text; [`CiRoute`]'s display
//! percent-encodes bytes outside the unreserved set.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use std::{fmt, mem::size_of};

use omp_core::{Str, StrMut};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};
use thiserror::Error;

use crate::CiProvider;

/// Longest details URL [`route`] accepts, in bytes.
pub const MAX_URL_LEN: usize = 4096;

/// Longest host, in bytes. This is the DNS limit.
pub const MAX_HOST_LEN: usize = 253;

/// Longest one decoded path segment, in bytes.
pub const MAX_SEGMENT_LEN: usize = 255;

/// Most path segments one details URL may name.
pub const MAX_SEGMENTS: usize = 32;

/// `http` or `https`.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Display,
	EnumString,
	IntoStaticStr,
	Serialize,
	Deserialize,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
#[serde(rename_all = "snake_case")]
pub enum RouteScheme {
	/// `http`.
	Http,
	/// `https`.
	Https,
}

/// A details URL classified into a provider and a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CiRoute {
	/// `http` or `https`.
	pub scheme:   RouteScheme,
	/// Provider. The id is the lowercased host.
	pub provider: CiProvider,
	/// Port, when the URL named one.
	pub port:     Option<u16>,
	/// Decoded path without a leading slash. Empty when the URL had none.
	pub path:     Str,
	/// Raw query without `?`. Empty when the URL had none. Not interpreted.
	pub query:    Str,
}

const _: () = assert!(size_of::<CiRoute>() <= 96, "CiRoute must stay compact");

impl CiRoute {
	/// The provider host, lowercased.
	#[must_use]
	pub fn host(&self) -> &str {
		self.provider.id.as_str()
	}
}

impl fmt::Display for CiRoute {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}://", self.scheme)?;
		if self.provider.id.as_str().contains(':') {
			write!(formatter, "[{}]", self.provider.id)?;
		} else {
			formatter.write_str(self.provider.id.as_str())?;
		}
		if let Some(port) = self.port {
			write!(formatter, ":{port}")?;
		}
		if !self.path.as_str().is_empty() {
			formatter.write_str("/")?;
			write_encoded(formatter, self.path.as_str())?;
		}
		if !self.query.as_str().is_empty() {
			write!(formatter, "?{}", self.query)?;
		}
		Ok(())
	}
}

/// A details URL could not be classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum RouteError {
	/// The input was empty or only whitespace.
	#[error("ci details URL is empty")]
	Empty,
	/// The input exceeded [`MAX_URL_LEN`].
	#[error("ci details URL is {len} bytes, above {max}")]
	TooLong {
		/// The length that was rejected.
		len: usize,
		/// The cap that was exceeded.
		max: usize,
	},
	/// The input had no `://` separator.
	#[error("ci details URL has no scheme")]
	MissingScheme,
	/// The scheme was not `http` or `https`.
	#[error("ci details URL scheme is not http or https")]
	UnsupportedScheme,
	/// The URL named no host.
	#[error("ci details URL is missing a host")]
	MissingHost,
	/// The URL carried userinfo (`user` or `user:secret` before `@`).
	///
	/// The error does not echo that text.
	#[error("ci details URL must not carry userinfo")]
	Userinfo,
	/// The host was not a DNS name or an IPv6 address in brackets.
	#[error("ci details URL host is invalid")]
	InvalidHost,
	/// The port was empty, zero, or not a `u16`.
	#[error("ci details URL port is invalid")]
	InvalidPort,
	/// The path had more segments than [`MAX_SEGMENTS`].
	#[error("ci details URL path has {len} segments, above {max}")]
	TooManySegments {
		/// The count that was rejected, including the segment that did not fit.
		len: usize,
		/// The cap that was exceeded.
		max: usize,
	},
	/// A segment was `.` or `..`.
	#[error("ci details URL path contains an empty or relative segment")]
	Unsafe,
	/// Percent-decoding did not produce UTF-8 text.
	#[error("ci details URL path is not valid text")]
	Undecodable,
	/// A decoded segment contained a separator or a control character.
	#[error("ci details URL path contains a character that is not allowed")]
	Invalid,
}

/// Classifies one absolute details URL into a provider and a path.
///
/// # Errors
///
/// Returns [`RouteError`] when the scheme, host, port, or a path segment is
/// not valid, or when the URL carries userinfo.
pub fn route(details_url: &str) -> Result<CiRoute, RouteError> {
	let url = details_url.trim();
	if url.is_empty() {
		return Err(RouteError::Empty);
	}
	if url.len() > MAX_URL_LEN {
		return Err(RouteError::TooLong { len: url.len(), max: MAX_URL_LEN });
	}
	let (scheme, rest) = split_scheme(url)?;
	let rest = strip_fragment(rest);
	let (authority_and_path, query) = split_query(rest);
	let (authority, path) = split_authority(authority_and_path);
	let (host, port) = parse_authority(authority)?;
	let path = decode_path(path)?;
	Ok(CiRoute {
		scheme,
		provider: CiProvider { id: host },
		port,
		path,
		query: if query.is_empty() {
			Str::new_static("")
		} else {
			Str::new(query)
		},
	})
}

fn split_scheme(url: &str) -> Result<(RouteScheme, &str), RouteError> {
	let Some((scheme, rest)) = url.split_once("://") else {
		return Err(RouteError::MissingScheme);
	};
	if scheme.is_empty() {
		return Err(RouteError::UnsupportedScheme);
	}
	let scheme = scheme
		.parse::<RouteScheme>()
		.map_err(|_| RouteError::UnsupportedScheme)?;
	Ok((scheme, rest))
}

fn strip_fragment(rest: &str) -> &str {
	rest.split_once('#').map_or(rest, |(path, _)| path)
}

fn split_query(rest: &str) -> (&str, &str) {
	rest.split_once('?').unwrap_or((rest, ""))
}

fn split_authority(rest: &str) -> (&str, &str) {
	rest.split_once('/').unwrap_or((rest, ""))
}

fn parse_authority(authority: &str) -> Result<(Str, Option<u16>), RouteError> {
	if authority.is_empty() {
		return Err(RouteError::MissingHost);
	}
	if authority.as_bytes().contains(&b'@') {
		return Err(RouteError::Userinfo);
	}
	if let Some(rest) = authority.strip_prefix('[') {
		return parse_ipv6_authority(rest);
	}
	if let Some((host, port)) = authority.rsplit_once(':') {
		if host.is_empty() {
			return Err(RouteError::MissingHost);
		}
		return Ok((normalize_dns_host(host)?, Some(parse_port(port)?)));
	}
	Ok((normalize_dns_host(authority)?, None))
}

fn parse_ipv6_authority(rest: &str) -> Result<(Str, Option<u16>), RouteError> {
	let Some((inner, after)) = rest.split_once(']') else {
		return Err(RouteError::InvalidHost);
	};
	validate_ipv6(inner)?;
	let host = lowercase_ascii(inner)?;
	let port = match after {
		"" => None,
		text => {
			let Some(port) = text.strip_prefix(':') else {
				return Err(RouteError::InvalidHost);
			};
			Some(parse_port(port)?)
		},
	};
	Ok((host, port))
}

fn validate_ipv6(inner: &str) -> Result<(), RouteError> {
	if inner.is_empty() || inner.len() > 39 || inner.contains(":::") {
		return Err(RouteError::InvalidHost);
	}
	let (left, right) = if let Some((left, right)) = inner.split_once("::") {
		if right.contains("::") {
			return Err(RouteError::InvalidHost);
		}
		(left, Some(right))
	} else {
		(inner, None)
	};
	let left_len = count_groups(left)?;
	let right_len = match right {
		Some(right) => count_groups(right)?,
		None => 0,
	};
	let total = left_len + right_len;
	let ok = if right.is_some() {
		total < 8
	} else {
		total == 8
	};
	if !ok {
		return Err(RouteError::InvalidHost);
	}
	Ok(())
}

fn count_groups(text: &str) -> Result<usize, RouteError> {
	if text.is_empty() {
		return Ok(0);
	}
	let mut count = 0usize;
	for group in text.split(':') {
		if group.is_empty() || group.len() > 4 || !group.bytes().all(|byte| byte.is_ascii_hexdigit())
		{
			return Err(RouteError::InvalidHost);
		}
		count += 1;
	}
	Ok(count)
}

fn normalize_dns_host(host: &str) -> Result<Str, RouteError> {
	if host.is_empty() {
		return Err(RouteError::MissingHost);
	}
	if host.len() > MAX_HOST_LEN {
		return Err(RouteError::TooLong { len: host.len(), max: MAX_HOST_LEN });
	}
	let bytes = host.as_bytes();
	let mut label_start = 0usize;
	for (index, &byte) in bytes.iter().enumerate() {
		if byte == b'.' {
			check_label(&bytes[label_start..index])?;
			label_start = index + 1;
		} else if !is_dns_label_byte(byte.to_ascii_lowercase()) {
			return Err(RouteError::InvalidHost);
		}
	}
	check_label(&bytes[label_start..])?;
	lowercase_ascii(host)
}

const fn check_label(label: &[u8]) -> Result<(), RouteError> {
	if label.is_empty() || label.len() > 63 {
		return Err(RouteError::InvalidHost);
	}
	let first = label[0].to_ascii_lowercase();
	let last = label[label.len() - 1].to_ascii_lowercase();
	if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
		return Err(RouteError::InvalidHost);
	}
	Ok(())
}

const fn is_dns_label_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'-'
}

fn lowercase_ascii(text: &str) -> Result<Str, RouteError> {
	if text.len() > MAX_HOST_LEN {
		return Err(RouteError::TooLong { len: text.len(), max: MAX_HOST_LEN });
	}
	let mut buf = [0u8; MAX_HOST_LEN];
	for (index, byte) in text.bytes().enumerate() {
		buf[index] = byte.to_ascii_lowercase();
	}
	let lowered = std::str::from_utf8(&buf[..text.len()]).map_err(|_| RouteError::InvalidHost)?;
	Ok(Str::new(lowered))
}

fn parse_port(text: &str) -> Result<u16, RouteError> {
	if text.is_empty() || text.len() > 5 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
		return Err(RouteError::InvalidPort);
	}
	let port: u16 = text.parse().map_err(|_| RouteError::InvalidPort)?;
	if port == 0 {
		return Err(RouteError::InvalidPort);
	}
	Ok(port)
}

fn decode_path(path: &str) -> Result<Str, RouteError> {
	if path.is_empty() {
		return Ok(Str::new_static(""));
	}
	let mut out = StrMut::with_capacity(path.len());
	let mut count = 0usize;
	let mut first = true;
	for raw in path.split('/') {
		if raw.is_empty() {
			continue;
		}
		if count == MAX_SEGMENTS {
			return Err(RouteError::TooManySegments { len: count + 1, max: MAX_SEGMENTS });
		}
		let segment = decode_segment(raw)?;
		if !first {
			out.push('/');
		}
		first = false;
		out.push_str(segment.as_str());
		count += 1;
	}
	Ok(out.freeze())
}

fn decode_segment(raw: &str) -> Result<Str, RouteError> {
	let encoded_max = MAX_SEGMENT_LEN.saturating_mul(3);
	if raw.len() > encoded_max {
		return Err(RouteError::TooLong { len: raw.len(), max: encoded_max });
	}
	if !raw.as_bytes().contains(&b'%') {
		return finish_segment(raw);
	}
	let mut buf = [0u8; MAX_SEGMENT_LEN];
	let mut len = 0usize;
	let bytes = raw.as_bytes();
	let mut index = 0usize;
	while index < bytes.len() {
		let byte = if bytes[index] == b'%' {
			if index + 2 >= bytes.len() {
				return Err(RouteError::Undecodable);
			}
			let hi = hex_nibble(bytes[index + 1]).ok_or(RouteError::Undecodable)?;
			let lo = hex_nibble(bytes[index + 2]).ok_or(RouteError::Undecodable)?;
			index += 3;
			(hi << 4) | lo
		} else {
			let byte = bytes[index];
			index += 1;
			byte
		};
		if len == MAX_SEGMENT_LEN {
			return Err(RouteError::TooLong { len: len + 1, max: MAX_SEGMENT_LEN });
		}
		buf[len] = byte;
		len += 1;
	}
	let text = std::str::from_utf8(&buf[..len]).map_err(|_| RouteError::Undecodable)?;
	finish_segment(text)
}

fn finish_segment(text: &str) -> Result<Str, RouteError> {
	if text.is_empty() || text == "." || text == ".." {
		return Err(RouteError::Unsafe);
	}
	if text.len() > MAX_SEGMENT_LEN {
		return Err(RouteError::TooLong { len: text.len(), max: MAX_SEGMENT_LEN });
	}
	if text
		.bytes()
		.any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b'?' | b'#'))
	{
		return Err(RouteError::Invalid);
	}
	Ok(Str::new(text))
}

fn write_encoded(formatter: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
	for &byte in text.as_bytes() {
		if is_unreserved(byte) || byte == b'/' {
			write!(formatter, "{}", char::from(byte))?;
		} else {
			write!(formatter, "%{byte:02x}")?;
		}
	}
	Ok(())
}

const fn is_unreserved(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

const fn hex_nibble(byte: u8) -> Option<u8> {
	match byte {
		b'0'..=b'9' => Some(byte - b'0'),
		b'a'..=b'f' => Some(byte - b'a' + 10),
		b'A'..=b'F' => Some(byte - b'A' + 10),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn routed(url: &str) -> CiRoute {
		route(url).unwrap_or_else(|error| panic!("{url}: {error}"))
	}

	#[test]
	fn classifies_a_details_url_into_a_provider_and_a_path() {
		let resource = routed("https://ci.example/pipelines/9/jobs/3");
		assert_eq!(resource.scheme, RouteScheme::Https);
		assert_eq!(resource.host(), "ci.example");
		assert_eq!(resource.provider.id.as_str(), "ci.example");
		assert_eq!(resource.path.as_str(), "pipelines/9/jobs/3");
		assert!(resource.port.is_none());
		assert!(resource.query.as_str().is_empty());
		assert_eq!(routed(&resource.to_string()), resource);
	}

	#[test]
	fn lowercases_the_host_keeps_a_named_port_and_drops_the_fragment() {
		let resource = routed("HTTPS://CI.Example:8443/A/b?x=1#frag");
		assert_eq!(resource.scheme, RouteScheme::Https);
		assert_eq!(resource.host(), "ci.example");
		assert_eq!(resource.port, Some(8443));
		assert_eq!(resource.path.as_str(), "A/b");
		assert_eq!(resource.query.as_str(), "x=1");
		assert_eq!(resource.to_string(), "https://ci.example:8443/A/b?x=1");
		assert_eq!(routed("https://ci.example:443/a").port, Some(443));
	}

	#[test]
	fn percent_decoding_and_ipv6() {
		let resource = routed("https://ci.example/my%2Drun/a%20b");
		assert_eq!(resource.path.as_str(), "my-run/a b");
		assert_eq!(resource.to_string(), "https://ci.example/my-run/a%20b");
		assert_eq!(routed(&resource.to_string()), resource);
		let ipv6 = routed("https://[::1]:9/pipelines/1");
		assert_eq!(ipv6.host(), "::1");
		assert_eq!(ipv6.port, Some(9));
		assert_eq!(ipv6.path.as_str(), "pipelines/1");
		assert_eq!(ipv6.to_string(), "https://[::1]:9/pipelines/1");
		assert_eq!(routed("https://ci.example//a///b/").path.as_str(), "a/b");
	}

	#[test]
	fn rejects_userinfo_without_echoing_it_and_rejects_bad_urls() {
		let error = route("https://user:secret@ci.example/a").unwrap_err();
		assert_eq!(error, RouteError::Userinfo);
		assert!(!error.to_string().contains("secret"));
		assert_eq!(route("   "), Err(RouteError::Empty));
		assert_eq!(route("ci.example/a"), Err(RouteError::MissingScheme));
		assert_eq!(route("ftp://ci.example/a"), Err(RouteError::UnsupportedScheme));
		assert_eq!(route("https:///a"), Err(RouteError::MissingHost));
		assert_eq!(route("https://ci.example:0/a"), Err(RouteError::InvalidPort));
		assert_eq!(route("https://ci.example:/a"), Err(RouteError::InvalidPort));
		assert_eq!(route("https://ci.example/a/../b"), Err(RouteError::Unsafe));
		assert_eq!(route("https://ci.example/%2e%2e"), Err(RouteError::Unsafe));
		assert_eq!(route("https://ci.example/%"), Err(RouteError::Undecodable));
		assert_eq!(route("https://[::1::2]/a"), Err(RouteError::InvalidHost));
		assert_eq!(route("https://-bad.example/a"), Err(RouteError::InvalidHost));
		let long = format!("https://ci.example/{}", "a".repeat(MAX_URL_LEN));
		assert!(matches!(route(&long), Err(RouteError::TooLong { .. })));
	}
}
