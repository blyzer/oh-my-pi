//! Path-component bounds for forge refs.
//!
//! Host, owner, and repository name are opaque text with a shared shape.
//! The charset is ASCII so a ref can be compared without a vendor's
//! normalization rules.

#![allow(missing_docs, reason = "strum IntoStaticStr emits undocumented into_str")]

use omp_core::Str;
use strum::{Display, EnumString, IntoStaticStr};
use thiserror::Error;

/// Longest host, owner segment, repository name, or single path segment.
pub const MAX_COMPONENT_LEN: usize = 255;

/// Longest joined owner, including the slashes between segments.
pub const MAX_OWNER_LEN: usize = 1024;

/// Longest commit identity stored on a [`crate::CommitRef`].
pub const MAX_COMMIT_LEN: usize = 128;

/// Most path segments accepted in one resource URL, including the number.
pub const MAX_SEGMENTS: usize = 32;

/// Which part of a ref failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive, const_into_str)]
pub enum PathComponent {
	/// Forge host. Opaque; not a vendor name.
	Host,
	/// Owner or nested owner segment.
	Owner,
	/// Repository name.
	Name,
	/// A path segment whose role is not known yet.
	Segment,
	/// Commit identity.
	Commit,
}

/// A host, owner, name, or commit identity failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum ComponentError {
	/// The component was empty.
	#[error("forge path component {component} is empty")]
	Empty {
		/// Which component was empty.
		component: PathComponent,
	},
	/// The component exceeded its byte cap.
	#[error("forge path component {component} is longer than {max} bytes")]
	TooLong {
		/// Which component was too long.
		component: PathComponent,
		/// The length that was rejected.
		len:       usize,
		/// The cap it exceeded.
		max:       usize,
	},
	/// The component contained a character outside its charset.
	#[error("forge path component {component} contains a character that is not allowed")]
	Invalid {
		/// Which component was rejected.
		component: PathComponent,
	},
	/// A segment was `.`, `..`, or otherwise not a ref component.
	#[error("forge path contains an empty or relative segment")]
	Unsafe,
	/// Percent-decoding did not produce UTF-8 text.
	#[error("forge path component is not valid text")]
	Undecodable,
}

pub fn check_host(text: &str) -> Result<(), ComponentError> {
	check_charset(text, PathComponent::Host, MAX_COMPONENT_LEN, is_host_byte)
}

pub fn check_name(text: &str) -> Result<(), ComponentError> {
	check_charset(text, PathComponent::Name, MAX_COMPONENT_LEN, is_name_byte)
}

pub fn check_owner(text: &str) -> Result<(), ComponentError> {
	if text.is_empty() {
		return Err(ComponentError::Empty { component: PathComponent::Owner });
	}
	if text.len() > MAX_OWNER_LEN {
		return Err(ComponentError::TooLong {
			component: PathComponent::Owner,
			len:       text.len(),
			max:       MAX_OWNER_LEN,
		});
	}
	if text.starts_with('/') || text.ends_with('/') || text.contains("//") {
		return Err(ComponentError::Unsafe);
	}
	for segment in text.split('/') {
		check_charset(segment, PathComponent::Owner, MAX_COMPONENT_LEN, is_name_byte)?;
	}
	Ok(())
}

pub fn check_commit(text: &str) -> Result<(), ComponentError> {
	check_charset(text, PathComponent::Commit, MAX_COMMIT_LEN, |byte| byte.is_ascii_alphanumeric())
}

pub fn decode_segment(raw: &str) -> Result<Str, ComponentError> {
	let encoded_max = MAX_COMPONENT_LEN.saturating_mul(3);
	if raw.len() > encoded_max {
		return Err(ComponentError::TooLong {
			component: PathComponent::Segment,
			len:       raw.len(),
			max:       encoded_max,
		});
	}
	if !raw.as_bytes().contains(&b'%') {
		return finish_segment(raw);
	}
	let mut buf = [0u8; MAX_COMPONENT_LEN];
	let mut len = 0usize;
	let bytes = raw.as_bytes();
	let mut index = 0usize;
	while index < bytes.len() {
		let byte = if bytes[index] == b'%' {
			if index + 2 >= bytes.len() {
				return Err(ComponentError::Undecodable);
			}
			let hi = hex_nibble(bytes[index + 1]).ok_or(ComponentError::Undecodable)?;
			let lo = hex_nibble(bytes[index + 2]).ok_or(ComponentError::Undecodable)?;
			index += 3;
			(hi << 4) | lo
		} else {
			let byte = bytes[index];
			index += 1;
			byte
		};
		if len == MAX_COMPONENT_LEN {
			return Err(ComponentError::TooLong {
				component: PathComponent::Segment,
				len:       len + 1,
				max:       MAX_COMPONENT_LEN,
			});
		}
		buf[len] = byte;
		len += 1;
	}
	let text = std::str::from_utf8(&buf[..len]).map_err(|_| ComponentError::Undecodable)?;
	finish_segment(text)
}

fn finish_segment(text: &str) -> Result<Str, ComponentError> {
	if text.is_empty() {
		return Err(ComponentError::Empty { component: PathComponent::Segment });
	}
	if text == "." || text == ".." {
		return Err(ComponentError::Unsafe);
	}
	if text.len() > MAX_COMPONENT_LEN {
		return Err(ComponentError::TooLong {
			component: PathComponent::Segment,
			len:       text.len(),
			max:       MAX_COMPONENT_LEN,
		});
	}
	if text.contains('/') || text.contains('\\') || text.bytes().any(|byte| byte == 0) {
		return Err(ComponentError::Invalid { component: PathComponent::Segment });
	}
	Ok(Str::new(text))
}

fn check_charset(
	text: &str,
	component: PathComponent,
	max: usize,
	allowed: impl Fn(u8) -> bool,
) -> Result<(), ComponentError> {
	if text.is_empty() {
		return Err(ComponentError::Empty { component });
	}
	if text.len() > max {
		return Err(ComponentError::TooLong { component, len: text.len(), max });
	}
	if !text.bytes().all(allowed) {
		return Err(ComponentError::Invalid { component });
	}
	Ok(())
}

const fn is_name_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

const fn is_host_byte(byte: u8) -> bool {
	is_name_byte(byte) || matches!(byte, b':' | b'[' | b']')
}

const fn hex_nibble(byte: u8) -> Option<u8> {
	match byte {
		b'0'..=b'9' => Some(byte - b'0'),
		b'a'..=b'f' => Some(byte - b'a' + 10),
		b'A'..=b'F' => Some(byte - b'A' + 10),
		_ => None,
	}
}
