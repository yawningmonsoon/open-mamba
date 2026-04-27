use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainLog {
    pub id: Uuid,
    pub kind: ChainLogKind,
    pub task_id: Option<Uuid>,
    pub payload_hash: String,
    pub tx_hash: String,
    pub block_number: Option<u64>,
    pub chain_id: u64,
    pub submitted_at: DateTime<Utc>,
    pub confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainLogKind {
    /// Task billing event anchored on-chain
    BillingAnchor,
    /// Agent run completion event
    AgentRunCompletion,
    /// Encrypted audit log pointer
    AuditPointer,
}
