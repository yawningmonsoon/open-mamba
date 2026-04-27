//! Lake ops for workflow DAGs.
//!
//! A workflow is an ordered chain of task templates. A run is one
//! execution of that chain — the run tracks `current_step`, captures
//! outputs as it progresses, and links to the in-flight task envelope
//! via `current_task_id`.
//!
//! When a worker completes a task, the workflow advancer (in mamba-api)
//! looks up the run by `current_task_id`, appends the response, advances
//! the step counter, and dispatches the next step.

use crate::store::Lake;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One step in a workflow. Field names mirror `IngestRequest` so callers
/// can think of each step as a small ingest payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    pub project: String,
    pub assigned_agent: String,
    #[serde(default)]
    pub skill: Option<String>,
    pub model: String,
    /// Free-form payload string. The previous step's output is injected
    /// as `__previous` in the JSON envelope sent to the agent.
    pub payload: String,
    #[serde(default = "default_priority")]
    pub priority: u8,
}

fn default_priority() -> u8 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub run_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub workflow_id: String,
    pub status: String,
    pub current_step: i32,
    pub outputs: Vec<serde_json::Value>,
    pub initial_input: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub current_task_id: Option<Uuid>,
}

impl Lake {
    // ── Workflow CRUD ─────────────────────────────────────────────────────────

    pub fn upsert_workflow(&self, w: &Workflow) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM workflows WHERE id=?",
            duckdb::params![w.id],
        )?;
        let steps_json = serde_json::to_string(&w.steps)?;
        conn.execute(
            "INSERT INTO workflows
                (id, name, steps_json, enabled, created_at, last_run_at, run_count)
             VALUES (?,?,?,?,?,?,?)",
            duckdb::params![
                w.id,
                w.name,
                steps_json,
                w.enabled,
                w.created_at.to_rfc3339(),
                w.last_run_at.map(|t| t.to_rfc3339()),
                w.run_count,
            ],
        )?;
        Ok(())
    }

    pub fn get_workflow(&self, id: &str) -> Result<Option<Workflow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, steps_json, enabled,
                    CAST(created_at AS VARCHAR), CAST(last_run_at AS VARCHAR),
                    run_count
             FROM workflows WHERE id=?",
        )?;
        let row = stmt
            .query_map(duckdb::params![id], map_workflow)?
            .filter_map(Result::ok)
            .next();
        Ok(row)
    }

    pub fn list_workflows(&self) -> Result<Vec<Workflow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, steps_json, enabled,
                    CAST(created_at AS VARCHAR), CAST(last_run_at AS VARCHAR),
                    run_count
             FROM workflows ORDER BY created_at DESC",
        )?;
        let mut rows = Vec::new();
        for r in stmt.query_map([], map_workflow)? {
            match r {
                Ok(w) => rows.push(w),
                Err(e) => tracing::error!("workflow row mapping failed: {e}"),
            }
        }
        Ok(rows)
    }

    pub fn delete_workflow(&self, id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM workflows WHERE id=?",
            duckdb::params![id],
        )?;
        Ok(n)
    }

    pub fn note_workflow_run_started(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE workflows SET last_run_at=now(), run_count=run_count+1 WHERE id=?",
            duckdb::params![id],
        )?;
        Ok(())
    }

    // ── Run lifecycle ─────────────────────────────────────────────────────────

    pub fn insert_run(&self, run: &WorkflowRun) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let outputs = serde_json::to_string(&run.outputs)?;
        conn.execute(
            "INSERT INTO workflow_runs
                (run_id, workflow_id, status, current_step, outputs_json,
                 initial_input, started_at, completed_at, error, current_task_id)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                run.run_id,
                run.workflow_id,
                run.status,
                run.current_step,
                outputs,
                run.initial_input,
                run.started_at.to_rfc3339(),
                run.completed_at.map(|t| t.to_rfc3339()),
                run.error,
                run.current_task_id.map(|u| u.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT run_id, workflow_id, status, current_step, outputs_json,
                    initial_input,
                    CAST(started_at AS VARCHAR), CAST(completed_at AS VARCHAR),
                    error, CAST(current_task_id AS VARCHAR)
             FROM workflow_runs WHERE run_id=?",
        )?;
        let row = stmt
            .query_map(duckdb::params![run_id], map_run)?
            .filter_map(Result::ok)
            .next();
        Ok(row)
    }

    pub fn get_run_by_task(&self, task_id: Uuid) -> Result<Option<WorkflowRun>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT run_id, workflow_id, status, current_step, outputs_json,
                    initial_input,
                    CAST(started_at AS VARCHAR), CAST(completed_at AS VARCHAR),
                    error, CAST(current_task_id AS VARCHAR)
             FROM workflow_runs WHERE current_task_id=? AND status='running'",
        )?;
        let row = stmt
            .query_map(duckdb::params![task_id.to_string()], map_run)?
            .filter_map(Result::ok)
            .next();
        Ok(row)
    }

    pub fn list_runs(&self, workflow_id: Option<&str>, limit: i64) -> Result<Vec<WorkflowRun>> {
        let conn = self.conn.lock().unwrap();
        let sql = match workflow_id {
            Some(_) => format!(
                "SELECT run_id, workflow_id, status, current_step, outputs_json,
                        initial_input,
                        CAST(started_at AS VARCHAR), CAST(completed_at AS VARCHAR),
                        error, CAST(current_task_id AS VARCHAR)
                 FROM workflow_runs WHERE workflow_id=?
                 ORDER BY started_at DESC LIMIT {limit}"
            ),
            None => format!(
                "SELECT run_id, workflow_id, status, current_step, outputs_json,
                        initial_input,
                        CAST(started_at AS VARCHAR), CAST(completed_at AS VARCHAR),
                        error, CAST(current_task_id AS VARCHAR)
                 FROM workflow_runs ORDER BY started_at DESC LIMIT {limit}"
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = Vec::new();
        let iter = if let Some(wid) = workflow_id {
            stmt.query_map(duckdb::params![wid], map_run)?
        } else {
            stmt.query_map([], map_run)?
        };
        for r in iter {
            match r {
                Ok(run) => rows.push(run),
                Err(e) => tracing::error!("workflow_run row mapping failed: {e}"),
            }
        }
        Ok(rows)
    }

    /// Advance a run to the next step. Caller passes the new state — this
    /// is just a persistence helper, not the orchestration logic.
    pub fn update_run(&self, run: &WorkflowRun) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let outputs = serde_json::to_string(&run.outputs)?;
        conn.execute(
            "UPDATE workflow_runs
             SET status=?, current_step=?, outputs_json=?,
                 completed_at=?, error=?, current_task_id=?
             WHERE run_id=?",
            duckdb::params![
                run.status,
                run.current_step,
                outputs,
                run.completed_at.map(|t| t.to_rfc3339()),
                run.error,
                run.current_task_id.map(|u| u.to_string()),
                run.run_id,
            ],
        )?;
        Ok(())
    }
}

// ── Mappers ───────────────────────────────────────────────────────────────────

fn map_workflow(r: &duckdb::Row) -> duckdb::Result<Workflow> {
    let steps_json: String = r.get(2)?;
    let steps: Vec<WorkflowStep> =
        serde_json::from_str(&steps_json).map_err(map_err)?;
    Ok(Workflow {
        id: r.get(0)?,
        name: r.get(1)?,
        steps,
        enabled: r.get(3)?,
        created_at: crate::triggers_parse_ts(r.get::<_, String>(4)?)?,
        last_run_at: r
            .get::<_, Option<String>>(5)?
            .map(crate::triggers_parse_ts)
            .transpose()?,
        run_count: r.get::<_, u64>(6)?,
    })
}

fn map_run(r: &duckdb::Row) -> duckdb::Result<WorkflowRun> {
    let outputs_json: String = r.get(4)?;
    let outputs: Vec<serde_json::Value> =
        serde_json::from_str(&outputs_json).map_err(map_err)?;
    let task_id_str: Option<String> = r.get(9)?;
    let current_task_id = task_id_str
        .as_deref()
        .and_then(|s| if s.is_empty() { None } else { Uuid::parse_str(s).ok() });
    Ok(WorkflowRun {
        run_id: r.get(0)?,
        workflow_id: r.get(1)?,
        status: r.get(2)?,
        current_step: r.get(3)?,
        outputs,
        initial_input: r.get(5)?,
        started_at: crate::triggers_parse_ts(r.get::<_, String>(6)?)?,
        completed_at: r
            .get::<_, Option<String>>(7)?
            .map(crate::triggers_parse_ts)
            .transpose()?,
        error: r.get(8)?,
        current_task_id,
    })
}

fn map_err<E: std::fmt::Display>(e: E) -> duckdb::Error {
    duckdb::Error::FromSqlConversionFailure(
        0,
        duckdb::types::Type::Any,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        )),
    )
}
