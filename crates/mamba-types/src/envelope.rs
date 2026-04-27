use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Every unit of work flowing through the open-mamba bus.
/// Written by Claude Desktop / Opus / cron triggers.
/// Read by openfang agents and mamba-api.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEnvelope {
    pub id: Uuid,
    pub project: String,
    pub source: TaskSource,
    /// openfang agent name (e.g. "coder", "devops-lead")
    pub assigned_agent: String,
    /// openfang skill slug (e.g. "fix-collector")
    pub skill: Option<String>,
    /// Preferred inference model — "nemotron/taifoon", "claude-sonnet-4-6", etc.
    pub model: String,
    pub payload: String,
    /// 1 = highest priority
    pub priority: u8,
    pub status: TaskStatus,
    /// openfang CronJob id once dispatched
    pub openfang_job_id: Option<Uuid>,
    /// tokens consumed (populated on completion via webhook)
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
    /// AES-GCM encrypted snapshot of the full payload (for audit log)
    pub encrypted_payload: Option<String>,
    /// on-chain tx hash of the billing event
    pub chain_tx: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl TaskEnvelope {
    pub fn new(
        project: impl Into<String>,
        source: TaskSource,
        assigned_agent: impl Into<String>,
        skill: Option<String>,
        model: impl Into<String>,
        payload: impl Into<String>,
        priority: u8,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            project: project.into(),
            source,
            assigned_agent: assigned_agent.into(),
            skill,
            model: model.into(),
            payload: payload.into(),
            priority,
            status: TaskStatus::Pending,
            openfang_job_id: None,
            tokens_in: None,
            tokens_out: None,
            cost_usd: None,
            encrypted_payload: None,
            chain_tx: None,
            created_at: Utc::now(),
            dispatched_at: None,
            completed_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Dispatched,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    ClaudeDesktop,
    ClaudeOpus,
    Cron,
    Api,
    Webhook,
}
