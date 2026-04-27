use anyhow::Result;
use mamba_chain::{anchor::ChainAnchor, crypto::Encryptor};
use mamba_types::{
    billing::{BillingEvent, BillingRecord},
    envelope::TaskEnvelope,
};
use tracing::info;
use uuid::Uuid;

pub struct BillingWorker {
    encryptor: Encryptor,
    anchor: ChainAnchor,
}

impl BillingWorker {
    pub fn from_env() -> Self {
        Self {
            encryptor: Encryptor::from_env().expect("MAMBA_ENCRYPT_KEY invalid"),
            anchor: ChainAnchor::from_env(),
        }
    }

    pub async fn process(
        &self,
        envelope: &TaskEnvelope,
        tokens_in: i64,
        tokens_out: i64,
        cost_usd: f64,
    ) -> Result<BillingRecord> {
        // 1. encrypt the full payload
        let full_json = serde_json::to_string(envelope)?;
        let encrypted_log = self.encryptor.encrypt(&full_json)?;
        let payload_hash = Encryptor::hash(&full_json);

        let event = BillingEvent {
            task_id: envelope.id,
            project: envelope.project.clone(),
            agent_slug: envelope.assigned_agent.clone(),
            model: envelope.model.clone(),
            tokens_in,
            tokens_out,
            cost_usd,
            payload_hash: payload_hash.clone(),
            encrypted_log: encrypted_log.clone(),
        };

        // 2. anchor on-chain
        let chain_tx = self.anchor.anchor(&event).await?;
        info!(
            task_id = %envelope.id,
            chain_tx = ?chain_tx,
            tokens_in,
            tokens_out,
            cost_usd,
            "billing anchored"
        );

        let record = BillingRecord {
            id: Uuid::new_v4(),
            task_id: envelope.id,
            project: envelope.project.clone(),
            agent_slug: envelope.assigned_agent.clone(),
            model: envelope.model.clone(),
            tokens_in,
            tokens_out,
            cost_usd,
            encrypted_log,
            chain_tx,
            chain_block: None,
            created_at: chrono::Utc::now(),
        };

        Ok(record)
    }
}
