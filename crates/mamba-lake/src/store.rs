use crate::schema::INIT_SQL;
use anyhow::Result;
use duckdb::Connection;
use mamba_types::{
    billing::BillingRecord,
    chain::ChainLog,
    envelope::{TaskEnvelope, TaskStatus},
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolverOutcomeRow {
    pub ts: String,
    pub intent_id: String,
    pub protocol: String,
    pub src_chain: u64,
    pub dst_chain: u64,
    pub decision: String,
    pub tx_hash: Option<String>,
    pub predicted_gas: Option<u64>,
    pub actual_gas: Option<u64>,
    pub effective_gas_price_wei: Option<String>,
    pub predicted_profit_usd: Option<f64>,
    pub actual_profit_usd: Option<f64>,
    pub skip_reason: Option<String>,
    pub error: Option<String>,
}

fn map_outcome_row(r: &duckdb::Row) -> duckdb::Result<SolverOutcomeRow> {
    Ok(SolverOutcomeRow {
        ts:                       r.get(0)?,
        intent_id:                r.get(1)?,
        protocol:                 r.get(2)?,
        src_chain:                r.get(3)?,
        dst_chain:                r.get(4)?,
        decision:                 r.get(5)?,
        tx_hash:                  r.get(6)?,
        predicted_gas:            r.get(7)?,
        actual_gas:               r.get(8)?,
        effective_gas_price_wei:  r.get(9)?,
        predicted_profit_usd:     r.get(10)?,
        actual_profit_usd:        r.get(11)?,
        skip_reason:              r.get(12)?,
        error:                    r.get(13)?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolverSkipRuleRow {
    pub id: String,
    pub protocol: String,
    pub rule_json: String,
    pub description: Option<String>,
    pub confidence: f64,
    pub sample_size: u64,
    pub active: bool,
    pub created_at: String,
    pub superseded_at: Option<String>,
}

fn map_skip_rule_row(r: &duckdb::Row) -> duckdb::Result<SolverSkipRuleRow> {
    Ok(SolverSkipRuleRow {
        id:            r.get(0)?,
        protocol:      r.get(1)?,
        rule_json:     r.get(2)?,
        description:   r.get(3)?,
        confidence:    r.get(4)?,
        sample_size:   r.get(5)?,
        active:        r.get(6)?,
        created_at:    r.get(7)?,
        superseded_at: r.get(8)?,
    })
}

#[derive(Debug, Serialize)]
pub struct TaskRow {
    pub id: String,
    pub project: String,
    pub source: String,
    pub assigned_agent: String,
    pub skill: Option<String>,
    pub model: String,
    pub payload: String,
    pub priority: i8,
    pub status: String,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
    pub created_at: String,
    pub dispatched_at: Option<String>,
    pub completed_at: Option<String>,
}

fn map_task_row(r: &duckdb::Row) -> duckdb::Result<TaskRow> {
    Ok(TaskRow {
        id:             r.get(0)?,
        project:        r.get(1)?,
        source:         r.get(2)?,
        assigned_agent: r.get(3)?,
        skill:          r.get(4)?,
        model:          r.get(5)?,
        payload:        r.get(6)?,
        priority:       r.get(7)?,
        status:         r.get(8)?,
        tokens_in:      r.get(9)?,
        tokens_out:     r.get(10)?,
        cost_usd:       r.get(11)?,
        created_at:     r.get(12)?,
        dispatched_at:  r.get(13)?,
        completed_at:   r.get(14)?,
    })
}

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
        // Named columns (not VALUES with positional placeholders) so we can
        // add columns to task_envelopes without breaking inserts.
        conn.execute(
            "INSERT INTO task_envelopes
                (id, project, source, assigned_agent, skill, model, payload,
                 priority, status, openfang_job_id, tokens_in, tokens_out,
                 cost_usd, encrypted_payload, chain_tx, created_at,
                 dispatched_at, completed_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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

    /// Persist the agent's response text. Called separately from
    /// `complete_envelope` so existing call sites don't need to thread
    /// the response through.
    pub fn save_response(&self, id: Uuid, response: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE task_envelopes SET response=? WHERE id=?",
            duckdb::params![response, id.to_string()],
        )?;
        Ok(())
    }

    /// Fetch the response text written by the agent (or None if the task
    /// hasn't completed or the response wasn't captured).
    pub fn get_response(&self, id: Uuid) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT response FROM task_envelopes WHERE id=?")?;
        let row = stmt
            .query_map(duckdb::params![id.to_string()], |r| r.get::<_, Option<String>>(0))?
            .filter_map(Result::ok)
            .next()
            .flatten();
        Ok(row)
    }

    pub fn query_tasks(
        &self,
        project: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<TaskRow>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT CAST(id AS VARCHAR), project, source, assigned_agent, skill, model, payload,
                    priority, status, tokens_in, tokens_out, cost_usd,
                    CAST(created_at AS VARCHAR), CAST(dispatched_at AS VARCHAR), CAST(completed_at AS VARCHAR)
             FROM task_envelopes"
        );
        let mut clauses: Vec<String> = Vec::new();
        if let Some(p) = project { clauses.push(format!("project='{}'", p.replace('\'', "''"))); }
        if let Some(s) = status  { clauses.push(format!("status='{}'",  s.replace('\'', "''"))); }
        if !clauses.is_empty() { sql.push_str(" WHERE "); sql.push_str(&clauses.join(" AND ")); }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {limit}"));
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<TaskRow> = stmt
            .query_map([], map_task_row)?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn debug_count_all(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let n = conn.query_row("SELECT COUNT(*) FROM task_envelopes", [], |r: &duckdb::Row| r.get(0))?;
        Ok(n)
    }

    pub fn debug_status_counts(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM task_envelopes GROUP BY status")?;
        let rows = stmt.query_map([], |r: &duckdb::Row| {
            let s: String = r.get(0)?;
            let c: i64 = r.get(1)?;
            Ok(format!("{s}={c}"))
        })?.filter_map(Result::ok).collect();
        Ok(rows)
    }

    pub fn get_task(&self, id: Uuid) -> Result<Option<TaskRow>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT CAST(id AS VARCHAR), project, source, assigned_agent, skill, model, payload,
                    priority, status, tokens_in, tokens_out, cost_usd,
                    CAST(created_at AS VARCHAR), CAST(dispatched_at AS VARCHAR), CAST(completed_at AS VARCHAR)
             FROM task_envelopes WHERE id='{}'",
            id
        );
        let mut stmt = conn.prepare(&sql)?;
        let row = stmt
            .query_map([], map_task_row)?
            .filter_map(Result::ok)
            .next();
        Ok(row)
    }

    /// Bulk-reset task rows back to `pending` so the worker re-dispatches
    /// them. Returns the number of rows updated.
    pub fn reset_to_pending(&self, ids: &[Uuid]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().unwrap();
        let mut updated = 0usize;
        for id in ids {
            let n = conn.execute(
                "UPDATE task_envelopes SET status='pending', dispatched_at=NULL, completed_at=NULL WHERE id=?",
                duckdb::params![id.to_string()],
            )?;
            updated += n;
        }
        Ok(updated)
    }

    /// Reset every non-completed task to pending. Useful after a server
    /// restart when tasks may have been mid-flight.
    pub fn reset_stuck_tasks(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE task_envelopes SET status='pending', dispatched_at=NULL, completed_at=NULL \
             WHERE status IN ('dispatched','running','failed')",
            duckdb::params![],
        )?;
        Ok(n)
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

    // ── Solver outcome ops ────────────────────────────────────────────────────

    pub fn insert_outcome(&self, o: &SolverOutcomeRow) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO solver_outcomes
                (ts, intent_id, protocol, src_chain, dst_chain, decision, tx_hash,
                 predicted_gas, actual_gas, effective_gas_price_wei,
                 predicted_profit_usd, actual_profit_usd, skip_reason, error)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                o.ts.clone(),
                o.intent_id,
                o.protocol,
                o.src_chain,
                o.dst_chain,
                o.decision,
                o.tx_hash,
                o.predicted_gas,
                o.actual_gas,
                o.effective_gas_price_wei,
                o.predicted_profit_usd,
                o.actual_profit_usd,
                o.skip_reason,
                o.error,
            ],
        )?;
        Ok(())
    }

    pub fn count_outcomes(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM solver_outcomes", [], |r| r.get(0))?;
        Ok(n)
    }

    pub fn recent_outcomes(&self, limit: i64) -> Result<Vec<SolverOutcomeRow>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT CAST(ts AS VARCHAR), intent_id, protocol, src_chain, dst_chain,
                    decision, tx_hash, predicted_gas, actual_gas, effective_gas_price_wei,
                    predicted_profit_usd, actual_profit_usd, skip_reason, error
             FROM solver_outcomes ORDER BY ts DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], map_outcome_row)?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    // ── Solver skip-rule ops ──────────────────────────────────────────────────

    pub fn insert_skip_rule(&self, r: &SolverSkipRuleRow) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO solver_skip_rules
                (id, protocol, rule_json, description, confidence, sample_size,
                 active, created_at, superseded_at)
             VALUES (?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                r.id,
                r.protocol,
                r.rule_json,
                r.description,
                r.confidence,
                r.sample_size,
                r.active,
                r.created_at,
                r.superseded_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_active_skip_rules(&self) -> Result<Vec<SolverSkipRuleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT CAST(id AS VARCHAR), protocol, rule_json, description, confidence,
                    sample_size, active, CAST(created_at AS VARCHAR),
                    CAST(superseded_at AS VARCHAR)
             FROM solver_skip_rules WHERE active=TRUE ORDER BY created_at DESC"
        )?;
        let rows = stmt
            .query_map([], map_skip_rule_row)?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    /// Mark all currently-active rules for `protocol` as superseded, then insert
    /// `new_rules`. Used by the weekly nemotron analyzer to publish a fresh set.
    pub fn replace_skip_rules(&self, protocol: &str, new_rules: &[SolverSkipRuleRow]) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE solver_skip_rules SET active=FALSE, superseded_at=now()
             WHERE protocol=? AND active=TRUE",
            duckdb::params![protocol],
        )?;
        for r in new_rules {
            conn.execute(
                "INSERT INTO solver_skip_rules
                    (id, protocol, rule_json, description, confidence, sample_size,
                     active, created_at, superseded_at)
                 VALUES (?,?,?,?,?,?,?,?,?)",
                duckdb::params![
                    r.id,
                    r.protocol,
                    r.rule_json,
                    r.description,
                    r.confidence,
                    r.sample_size,
                    r.active,
                    r.created_at,
                    r.superseded_at,
                ],
            )?;
        }
        Ok(new_rules.len())
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
