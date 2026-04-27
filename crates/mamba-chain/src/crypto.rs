//! AES-256-GCM encryption for audit log payloads.
//! Key stored in MAMBA_ENCRYPT_KEY env var (32-byte hex).

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub struct Encryptor {
    cipher: Aes256Gcm,
}

impl Encryptor {
    pub fn from_hex_key(hex_key: &str) -> Result<Self> {
        let bytes = hex::decode(hex_key)?;
        if bytes.len() != 32 {
            bail!("MAMBA_ENCRYPT_KEY must be 32 bytes (64 hex chars)");
        }
        let key = Key::<Aes256Gcm>::from_slice(&bytes);
        Ok(Self { cipher: Aes256Gcm::new(key) })
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("MAMBA_ENCRYPT_KEY")
            .unwrap_or_else(|_| {
                // dev-only deterministic key — override in prod
                "0000000000000000000000000000000000000000000000000000000000000001".to_string()
            });
        Self::from_hex_key(&key)
    }

    /// Encrypt plaintext → base64(nonce || ciphertext)
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self.cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encrypt failed: {e}"))?;
        let mut blob = nonce_bytes.to_vec();
        blob.extend(ciphertext);
        Ok(B64.encode(blob))
    }

    /// SHA-256 hash of payload as hex string.
    pub fn hash(payload: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        hex::encode(hasher.finalize())
    }
}
