use crate::schema::INIT_SQL;
use anyhow::Result;
use duckdb::Connection;
use mamba_types::{
    billing::BillingRecord,
    chain::ChainLog,
    envelope::{TaskEnvelope, TaskStatus},
};

use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;
use uuid::Uuid;

/// Thread-safe DuckDB data lake.
/// Uses blocking Mutex: DuckDB Connection is not Send+Sync natively.
#[derive(Clone)]
pub struct Lake {
    pub(crate) conn: Arc<Mutex<Connection>>,
}

impl Lake {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(INIT_SQL)?;
        info!("DuckDB lake initialized");
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(INIT_SQL)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    // ── Envelope ops ──────────────────────────────────────────────────────────

    pub fn insert_envelope(&self, e: &TaskEnvelope) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO task_envelopes VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                e.id.to_string(),
                e.project,
                e.source.as_str(),
                e.assigned_agent,
                e.skill,
                e.model,
                e.payload,
                e.priority,
                e.status.as_str(),
                e.openfang_job_id.map(|u| u.to_string()),
                e.tokens_in,
                e.tokens_out,
                e.cost_usd,
                e.encrypted_payload,
                e.chain_tx,
                e.created_at.to_rfc3339(),
                e.dispatched_at.map(|t| t.to_rfc3339()),
                e.completed_at.map(|t| t.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn update_status(&self, id: Uuid, status: TaskStatus, openfang_job_id: Option<Uuid>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE task_envelopes SET status=?, openfang_job_id=?, dispatched_at=now()
             WHERE id=?",
            duckdb::params![
                status.as_str(),
                openfang_job_id.map(|u| u.to_string()),
                id.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn complete_envelope(
        &self,
        id: Uuid,
        tokens_in: i64,
        tokens_out: i64,
        cost_usd: f64,
        chain_tx: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE task_envelopes
             SET status='done', tokens_in=?, tokens_out=?, cost_usd=?,
                 chain_tx=?, completed_at=now()
             WHERE id=?",
            duckdb::params![tokens_in, tokens_out, cost_usd, chain_tx, id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_pending(&self) -> Result<Vec<Uuid>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id FROM task_envelopes WHERE status='pending' ORDER BY priority ASC, created_at ASC"
        )?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|s| Uuid::parse_str(&s).ok())
            .collect();
        Ok(ids)
    }

    // ── Billing ops ───────────────────────────────────────────────────────────

    pub fn insert_billing(&self, b: &BillingRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO billing_records VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                b.id.to_string(),
                b.task_id.to_string(),
                b.project,
                b.agent_slug,
                b.model,
                b.tokens_in,
                b.tokens_out,
                b.cost_usd,
                b.encrypted_log,
                b.chain_tx,
                b.chain_block,
                b.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn update_billing_tx(&self, id: Uuid, tx: &str, block: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE billing_records SET chain_tx=?, chain_block=? WHERE id=?",
            duckdb::params![tx, block, id.to_string()],
        )?;
        Ok(())
    }

    // ── Chain log ops ─────────────────────────────────────────────────────────

    pub fn insert_chain_log(&self, log: &ChainLog) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chain_logs VALUES (?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                log.id.to_string(),
                log.kind.as_str(),
                log.task_id.map(|u| u.to_string()),
                log.payload_hash,
                log.tx_hash,
                log.block_number,
                log.chain_id,
                log.submitted_at.to_rfc3339(),
                log.confirmed_at.map(|t| t.to_rfc3339()),
            ],
        )?;
        Ok(())
    }
}
