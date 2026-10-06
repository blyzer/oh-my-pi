//! Human-readable account names.
//!
//! A name is a handle the user chooses for one stored account so the CLI and
//! `/pin` can address it without typing an account id. It is unique within one
//! provider, lives only in the account state store, and never reaches the
//! session journal: a pin records the opaque credential-affinity digest, not
//! the name.

use omp_core::Str;

/// Longest accepted account name, in characters.
pub const MAX_ACCOUNT_NAME_LEN: usize = 64;

/// A validated account name: lowercase ASCII letters, digits, `-` and `_`,
/// starting with a letter or digit, at most [`MAX_ACCOUNT_NAME_LEN`] long.
///
/// The alphabet excludes `:` and `/`, so a name can never collide with an
/// account id (`provider:principal`) or read as a `provider/name` selector.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AccountName(Str);

/// Why text is not a valid [`AccountName`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AccountNameError {
	/// The name is empty.
	#[error("an account name must not be empty")]
	Empty,
	/// The name is longer than [`MAX_ACCOUNT_NAME_LEN`] characters.
	#[error("an account name is at most 64 characters")]
	TooLong,
	/// The name starts with `-` or `_`.
	#[error("an account name must start with a lowercase letter or digit")]
	BadStart,
	/// The name contains a character outside the alphabet.
	#[error("an account name may contain only lowercase letters, digits, '-' and '_'")]
	BadChar {
		/// The first offending character.
		found: char,
	},
}

impl AccountName {
	/// Validates `text` as an account name.
	pub fn parse(text: &str) -> Result<Self, AccountNameError> {
		let Some(first) = text.chars().next() else {
			return Err(AccountNameError::Empty);
		};
		if text.chars().count() > MAX_ACCOUNT_NAME_LEN {
			return Err(AccountNameError::TooLong);
		}
		if let Some(found) = text
			.chars()
			.find(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_')))
		{
			return Err(AccountNameError::BadChar { found });
		}
		if matches!(first, '-' | '_') {
			return Err(AccountNameError::BadStart);
		}
		Ok(Self(Str::new(text)))
	}

	/// Borrows the validated name text.
	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}

	/// Moves the validated name into its shared string.
	pub fn into_inner(self) -> Str {
		self.0
	}
}

/// Why a selector did not resolve to exactly one stored account.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AccountSelectError {
	/// No stored account has this id or name.
	#[error("no stored account matches the selector")]
	Unknown,
	/// The bare name is used by accounts of several providers; qualify it as
	/// `provider/name`.
	#[error("the account name is used by {providers} providers; write it as provider/name")]
	Ambiguous {
		/// How many providers have an account with this name.
		providers: usize,
	},
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn accepts_the_documented_alphabet_and_bounds() {
		for ok in ["work", "a", "9lives", "team_a-2", &"x".repeat(MAX_ACCOUNT_NAME_LEN)] {
			assert_eq!(AccountName::parse(ok).expect(ok).as_str(), ok);
		}
	}

	#[test]
	fn rejects_everything_else_with_the_reason() {
		assert_eq!(AccountName::parse(""), Err(AccountNameError::Empty));
		assert_eq!(
			AccountName::parse(&"x".repeat(MAX_ACCOUNT_NAME_LEN + 1)),
			Err(AccountNameError::TooLong)
		);
		assert_eq!(AccountName::parse("-work"), Err(AccountNameError::BadStart));
		assert_eq!(AccountName::parse("_work"), Err(AccountNameError::BadStart));
		assert_eq!(AccountName::parse("Work"), Err(AccountNameError::BadChar { found: 'W' }));
		assert_eq!(AccountName::parse("a:b"), Err(AccountNameError::BadChar { found: ':' }));
		assert_eq!(AccountName::parse("a/b"), Err(AccountNameError::BadChar { found: '/' }));
		assert_eq!(AccountName::parse("a b"), Err(AccountNameError::BadChar { found: ' ' }));
		assert_eq!(AccountName::parse("é"), Err(AccountNameError::BadChar { found: 'é' }));
	}
}
