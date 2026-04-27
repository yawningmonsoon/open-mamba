use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One billing record per task completion — written to DuckDB + submitted on-chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingRecord {
    pub id: Uuid,
    pub task_id: Uuid,
    pub project: String,
    pub agent_slug: String,
    pub model: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: f64,
    /// AES-GCM encrypted JSON of the full envelope payload
    pub encrypted_log: String,
    /// on-chain tx hash (rpc.taifoon.dev)
    pub chain_tx: Option<String>,
    pub chain_block: Option<u64>,
    pub created_at: DateTime<Utc>,
}

/// Raw event emitted before chain submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingEvent {
    pub task_id: Uuid,
    pub project: String,
    pub agent_slug: String,
    pub model: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: f64,
    pub payload_hash: String,
    pub encrypted_log: String,
}
