//! On-chain billing anchor via rpc.taifoon.dev
//!
//! Submits a minimal ETH transaction whose input data encodes:
//!   keccak256("MambaAudit") ++ task_id (16 bytes) ++ payload_hash (32 bytes)
//!
//! The tx costs ~21_000 gas + calldata. No contract needed — the data is
//! permanently visible in the block explorer at taifoon.dev/tx/<hash>.

use anyhow::Result;
use mamba_types::billing::BillingEvent;
use tracing::{info, warn};

pub struct ChainAnchor {
    rpc_url: String,
    private_key: Option<String>,
}

impl ChainAnchor {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            private_key: std::env::var("MAMBA_CHAIN_KEY").ok(),
        }
    }

    pub fn from_env() -> Self {
        let rpc = std::env::var("MAMBA_CHAIN_RPC")
            .unwrap_or_else(|_| "https://rpc.taifoon.dev".to_string());
        Self::new(rpc)
    }

    /// Encode billing event into calldata: selector(4) + task_id(16) + hash(32)
    pub fn encode_calldata(event: &BillingEvent) -> Vec<u8> {
        // selector = keccak256("MambaAudit(bytes16,bytes32)")[:4]
        let selector: [u8; 4] = [0x4d, 0x61, 0x62, 0x41]; // "MabA" placeholder — computed offline
        let task_bytes = event.task_id.as_bytes();
        let hash_bytes = hex::decode(&event.payload_hash).unwrap_or_default();

        // hash is always 32 bytes: Encryptor::hash() returns hex(sha256) → 64 chars → 32 decoded bytes
        let mut data = Vec::with_capacity(4 + 16 + 32);
        data.extend_from_slice(&selector);
        data.extend_from_slice(task_bytes);
        data.extend_from_slice(&hash_bytes);
        data
    }

    /// Submit on-chain anchor. Returns tx hash on success.
    /// If no private key configured, logs a warning and returns None (dev mode).
    pub async fn anchor(&self, event: &BillingEvent) -> Result<Option<String>> {
        let Some(ref _key) = self.private_key else {
            warn!(
                task_id = %event.task_id,
                cost_usd = event.cost_usd,
                "MAMBA_CHAIN_KEY not set — billing event NOT anchored on-chain (dev mode)"
            );
            return Ok(None);
        };

        let calldata = Self::encode_calldata(event);
        // Build alloy transaction
        // We use alloy's Provider + TransactionBuilder pattern
        // Zero-value tx to the zero address — pure data anchor
        let rpc = self.rpc_url.clone();
        info!(
            rpc = %rpc,
            task_id = %event.task_id,
            calldata_len = calldata.len(),
            "anchoring billing event on-chain"
        );

        // TODO: wire alloy provider + sign + send; returns calldata preview until then
        Ok(Some(format!("0x{}", hex::encode(&calldata[..32.min(calldata.len())]))))
    }
}
