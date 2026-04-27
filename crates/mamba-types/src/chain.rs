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
    BillingAnchor,
    AgentRunCompletion,
    AuditPointer,
}

impl ChainLogKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BillingAnchor => "billing_anchor",
            Self::AgentRunCompletion => "agent_run_completion",
            Self::AuditPointer => "audit_pointer",
        }
    }
}
