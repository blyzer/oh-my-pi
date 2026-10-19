//! Ed25519 device key: enrollment, persistence, pairing code, and
//! challenge-response signing/verification.
//!
//! A device key is generated once and persisted to `~/.o2/device.key` (raw
//! 64-byte concatenation: 32-byte seed followed by 32-byte public key).
//! The human-typeable pairing code is the Crockford base32 encoding of the
//! first 10 bytes of the public key (16 characters, no padding).

use std::{
	fs,
	io::{self, Write as _},
	path::PathBuf,
};

use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};

/// Crockford base32 alphabet (uppercase, no padding).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encode bytes using Crockford base32 (no padding).
fn crockford_base32(input: &[u8]) -> String {
	let mut output = String::with_capacity((input.len() * 8 + 4) / 5);
	let mut bits: u32 = 0;
	let mut nbits: u32 = 0;
	for &byte in input {
		bits = (bits << 8) | u32::from(byte);
		nbits += 8;
		while nbits >= 5 {
			nbits -= 5;
			output.push(CROCKFORD[((bits >> nbits) & 0x1f) as usize] as char);
		}
	}
	if nbits > 0 {
		output.push(CROCKFORD[((bits << (5 - nbits)) & 0x1f) as usize] as char);
	}
	output
}

/// Ed25519 device key (keypair in PKCS#8 v2 DER form held by `ring`).
pub struct DeviceKey {
	pair:       Ed25519KeyPair,
	/// Raw 32-byte public key.
	public_key: [u8; 32],
	/// Path the key was loaded from or persisted to.
	path:       PathBuf,
}

impl DeviceKey {
	/// Generates a fresh keypair and persists it to `path`.
	fn generate_at(path: PathBuf) -> Result<Self, DeviceAuthError> {
		let rng = ring::rand::SystemRandom::new();
		let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| DeviceAuthError::InvalidKey)?;
		let pair =
			Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| DeviceAuthError::InvalidKey)?;
		let public_key = Self::extract_public_key(&pair)?;

		// Write atomically via a temp file in the same directory.
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		let tmp = path.with_extension("key.tmp");
		{
			let mut file = fs::OpenOptions::new()
				.write(true)
				.create(true)
				.truncate(true)
				.open(&tmp)?;
			file.write_all(pkcs8.as_ref())?;
			file.flush()?;
		}
		fs::rename(&tmp, &path)?;

		Ok(Self { pair, public_key, path })
	}

	/// Loads a keypair from `path` (raw PKCS#8 DER bytes).
	fn load_from(path: PathBuf) -> Result<Self, DeviceAuthError> {
		let bytes = fs::read(&path)?;
		let pair = Ed25519KeyPair::from_pkcs8(&bytes).map_err(|_| DeviceAuthError::InvalidKey)?;
		let public_key = Self::extract_public_key(&pair)?;
		Ok(Self { pair, public_key, path })
	}

	fn extract_public_key(pair: &Ed25519KeyPair) -> Result<[u8; 32], DeviceAuthError> {
		let bytes = pair.public_key().as_ref();
		bytes.try_into().map_err(|_| DeviceAuthError::InvalidKey)
	}

	/// Returns the path this key is persisted to.
	pub fn path(&self) -> &PathBuf {
		&self.path
	}

	/// Returns the raw 32-byte public key.
	pub fn public_key_bytes(&self) -> &[u8; 32] {
		&self.public_key
	}

	/// Returns the human-typeable pairing code: Crockford base32 of the first
	/// 10 bytes of the public key (16 characters, no separators).
	pub fn pairing_code(&self) -> String {
		crockford_base32(&self.public_key[..10])
	}

	/// Signs `challenge` with the device private key.
	pub fn sign_challenge(&self, challenge: &[u8]) -> [u8; 64] {
		let sig = self.pair.sign(challenge);
		sig.as_ref()
			.try_into()
			.expect("Ed25519 signature is always 64 bytes")
	}

	/// Verifies `sig` over `challenge` using `pubkey_bytes`.
	pub fn verify(pubkey_bytes: &[u8; 32], challenge: &[u8], sig: &[u8; 64]) -> bool {
		let pk = UnparsedPublicKey::new(&ED25519, pubkey_bytes.as_ref());
		pk.verify(challenge, sig.as_ref()).is_ok()
	}
}

/// Generates a fresh device key.
pub fn generate(path: PathBuf) -> Result<DeviceKey, DeviceAuthError> {
	DeviceKey::generate_at(path)
}

/// Loads the device key from `path`, generating and persisting a fresh one if
/// it does not exist yet.
///
/// Returns `(key, newly_generated)`.
pub fn load_or_generate(path: PathBuf) -> Result<(DeviceKey, bool), DeviceAuthError> {
	if path.exists() {
		DeviceKey::load_from(path).map(|k| (k, false))
	} else {
		DeviceKey::generate_at(path).map(|k| (k, true))
	}
}

/// Device authentication error.
#[derive(Debug, thiserror::Error)]
pub enum DeviceAuthError {
	/// A filesystem operation failed.
	#[error("device key I/O error")]
	Io(#[from] io::Error),
	/// The key material on disk is absent or corrupt.
	#[error("device key is invalid or could not be parsed")]
	InvalidKey,
	/// A challenge signature did not verify against the stored public key.
	#[error("device challenge signature did not match")]
	SignatureMismatch,
}

#[cfg(test)]
mod tests {
	use tempfile::tempdir;

	use super::*;

	#[test]
	fn round_trip_sign_verify() {
		let dir = tempdir().unwrap();
		let path = dir.path().join("device.key");
		let (key, new) = load_or_generate(path.clone()).unwrap();
		assert!(new);

		let challenge = b"hello-challenge-nonce-12345";
		let sig = key.sign_challenge(challenge);
		assert!(DeviceKey::verify(key.public_key_bytes(), challenge, &sig));
	}

	#[test]
	fn reload_yields_same_public_key() {
		let dir = tempdir().unwrap();
		let path = dir.path().join("device.key");
		let (k1, _) = load_or_generate(path.clone()).unwrap();
		let pub1 = *k1.public_key_bytes();
		drop(k1);

		let (k2, new) = load_or_generate(path).unwrap();
		assert!(!new);
		assert_eq!(pub1, *k2.public_key_bytes());
	}

	#[test]
	fn pairing_code_is_16_chars() {
		let dir = tempdir().unwrap();
		let path = dir.path().join("device.key");
		let (key, _) = load_or_generate(path).unwrap();
		let code = key.pairing_code();
		assert_eq!(code.len(), 16, "pairing code must be 16 Crockford chars");
		assert!(code.chars().all(|c| CROCKFORD.contains(&(c as u8))));
	}

	#[test]
	fn wrong_sig_fails_verify() {
		let dir = tempdir().unwrap();
		let path = dir.path().join("device.key");
		let (key, _) = load_or_generate(path).unwrap();
		let challenge = b"nonce";
		let mut sig = key.sign_challenge(challenge);
		sig[0] ^= 0xff;
		assert!(!DeviceKey::verify(key.public_key_bytes(), challenge, &sig));
	}
}
