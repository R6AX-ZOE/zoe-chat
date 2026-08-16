//! 身份层:用户 Ed25519 身份密钥 + BIP39 助记词备份。
//!
//! 指纹 = SHA-256(身份公钥)前 32 字节(Safety-Number 风格)。
//! 助记词 = 32 字节 Ed25519 种子作为 BIP39 熵(24 词)。

use bip39::{Language, Mnemonic};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const FINGERPRINT_LEN: usize = 32;
pub const SEED_LEN: usize = 32;

#[derive(Error, Debug)]
pub enum IdentityError {
    #[error("invalid mnemonic: {0}")]
    Mnemonic(#[from] bip39::Error),
    #[error("mnemonic entropy must be {SEED_LEN} bytes")]
    BadSeed,
}

/// 用户身份密钥对(Ed25519)。
#[derive(Clone)]
pub struct IdentityKeyPair {
    signing: SigningKey,
}

impl IdentityKeyPair {
    pub fn generate() -> Self {
        let mut csprng = OsRng;
        Self {
            signing: SigningKey::generate(&mut csprng),
        }
    }

    pub fn from_seed(seed: &[u8; SEED_LEN]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    pub fn seed(&self) -> [u8; SEED_LEN] {
        self.signing.to_bytes()
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// 身份指纹:SHA-256(公钥)前 32 字节。
    pub fn fingerprint(&self) -> [u8; FINGERPRINT_LEN] {
        let digest = Sha256::digest(self.verifying_key().to_bytes());
        let mut fp = [0u8; FINGERPRINT_LEN];
        fp.copy_from_slice(&digest[..FINGERPRINT_LEN]);
        fp
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing.sign(msg)
    }

    pub fn verify(vk: &VerifyingKey, msg: &[u8], sig: &Signature) -> bool {
        vk.verify_strict(msg, sig).is_ok()
    }

    /// 导出为 24 词助记词(种子即 BIP39 熵)。
    pub fn to_mnemonic(&self) -> String {
        let mnemonic =
            Mnemonic::from_entropy(&self.seed()).expect("32B entropy is valid");
        mnemonic.to_string()
    }

    pub fn from_mnemonic(phrase: &str) -> Result<Self, IdentityError> {
        let mnemonic = Mnemonic::parse_in(Language::English, phrase)?;
        let entropy = mnemonic.to_entropy();
        if entropy.len() != SEED_LEN {
            return Err(IdentityError::BadSeed);
        }
        let mut seed = [0u8; SEED_LEN];
        seed.copy_from_slice(&entropy);
        Ok(Self::from_seed(&seed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonic_roundtrip() {
        let id = IdentityKeyPair::generate();
        let phrase = id.to_mnemonic();
        assert_eq!(phrase.split_whitespace().count(), 24);
        let restored = IdentityKeyPair::from_mnemonic(&phrase).unwrap();
        assert_eq!(restored.seed(), id.seed());
        assert_eq!(restored.fingerprint(), id.fingerprint());
    }

    #[test]
    fn bad_mnemonic_rejected() {
        assert!(IdentityKeyPair::from_mnemonic("not a mnemonic at all").is_err());
    }

    #[test]
    fn fingerprint_is_sha256_prefix() {
        let id = IdentityKeyPair::generate();
        let digest = Sha256::digest(id.verifying_key().to_bytes());
        assert_eq!(id.fingerprint(), digest[..FINGERPRINT_LEN]);
    }

    #[test]
    fn sign_verify() {
        let id = IdentityKeyPair::generate();
        let msg = b"pairing handshake";
        let sig = id.sign(msg);
        assert!(IdentityKeyPair::verify(&id.verifying_key(), msg, &sig));
        assert!(!IdentityKeyPair::verify(&id.verifying_key(), b"tampered", &sig));
    }
}
