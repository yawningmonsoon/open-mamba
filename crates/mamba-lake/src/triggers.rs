//! Lake ops for webhook + cron triggers.
//!
//! Both kinds of trigger expand to the same shape — a task envelope.
//! `Webhook` rows are keyed by `hook_id` (a user-chosen string in the
//! URL). `Schedule` rows are keyed by `id` and hold a `next_run_at`
//! that the worker advances after each fire.

use crate::store::Lake;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub hook_id: String,
    pub project: String,
    pub assigned_agent: String,
    pub skill: Option<String>,
    pub model: String,
    /// Literal payload string. The current implementation uses this as-is
    /// when a request lands; future versions can run Liquid/Handlebars
    /// substitution against the request body.
    pub payload_template: String,
    pub priority: i8,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub fire_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    /// 5-field cron expression in UTC: `min hour dom month dow`.
    pub cron_expr: String,
    pub project: String,
    pub assigned_agent: String,
    pub skill: Option<String>,
    pub model: String,
    pub payload: String,
    pub priority: i8,
    pub enabled: bool,
    pub next_run_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub fire_count: u64,
    pub created_at: DateTime<Utc>,
}

impl Lake {
    // ── Webhooks ──────────────────────────────────────────────────────────────

    pub fn upsert_webhook(&self, h: &Webhook) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // DuckDB has no UPSERT before 1.0.x in some builds; emulate with
        // delete-then-insert. Webhook IDs are short, this is cheap.
        conn.execute(
            "DELETE FROM webhooks WHERE hook_id=?",
            duckdb::params![h.hook_id],
        )?;
        conn.execute(
            "INSERT INTO webhooks
                (hook_id, project, assigned_agent, skill, model, payload_template,
                 priority, enabled, created_at, last_fired_at, fire_count)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                h.hook_id,
                h.project,
                h.assigned_agent,
                h.skill,
                h.model,
                h.payload_template,
                h.priority,
                h.enabled,
                h.created_at.to_rfc3339(),
                h.last_fired_at.map(|t| t.to_rfc3339()),
                h.fire_count,
            ],
        )?;
        Ok(())
    }

    pub fn get_webhook(&self, hook_id: &str) -> Result<Option<Webhook>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT hook_id, project, assigned_agent, skill, model, payload_template,
                    priority, enabled,
                    CAST(created_at AS VARCHAR), CAST(last_fired_at AS VARCHAR),
                    fire_count
             FROM webhooks WHERE hook_id=?",
        )?;
        let row = stmt
            .query_map(duckdb::params![hook_id], map_webhook)?
            .filter_map(Result::ok)
            .next();
        Ok(row)
    }

    pub fn list_webhooks(&self) -> Result<Vec<Webhook>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT hook_id, project, assigned_agent, skill, model, payload_template,
                    priority, enabled,
                    CAST(created_at AS VARCHAR), CAST(last_fired_at AS VARCHAR),
                    fire_count
             FROM webhooks ORDER BY created_at DESC",
        )?;
        let mut rows = Vec::new();
        for r in stmt.query_map([], map_webhook)? {
            match r {
                Ok(row) => rows.push(row),
                Err(e) => tracing::error!("webhook row mapping failed: {e}"),
            }
        }
        Ok(rows)
    }

    pub fn delete_webhook(&self, hook_id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM webhooks WHERE hook_id=?",
            duckdb::params![hook_id],
        )?;
        Ok(n)
    }

    pub fn note_webhook_fired(&self, hook_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE webhooks SET last_fired_at=now(), fire_count=fire_count+1
             WHERE hook_id=?",
            duckdb::params![hook_id],
        )?;
        Ok(())
    }

    // ── Schedules ─────────────────────────────────────────────────────────────

    pub fn upsert_schedule(&self, s: &Schedule) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM schedules WHERE id=?",
            duckdb::params![s.id],
        )?;
        conn.execute(
            "INSERT INTO schedules
                (id, cron_expr, project, assigned_agent, skill, model, payload,
                 priority, enabled, next_run_at, last_fired_at, fire_count, created_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            duckdb::params![
                s.id,
                s.cron_expr,
                s.project,
                s.assigned_agent,
                s.skill,
                s.model,
                s.payload,
                s.priority,
                s.enabled,
                s.next_run_at.to_rfc3339(),
                s.last_fired_at.map(|t| t.to_rfc3339()),
                s.fire_count,
                s.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, cron_expr, project, assigned_agent, skill, model, payload,
                    priority, enabled,
                    CAST(next_run_at AS VARCHAR), CAST(last_fired_at AS VARCHAR),
                    fire_count, CAST(created_at AS VARCHAR)
             FROM schedules ORDER BY next_run_at ASC",
        )?;
        let rows = stmt
            .query_map([], map_schedule)?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn delete_schedule(&self, id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM schedules WHERE id=?",
            duckdb::params![id],
        )?;
        Ok(n)
    }

    /// Schedules whose `next_run_at <= now` and `enabled = true`. The
    /// trigger worker drains this list each tick.
    pub fn due_schedules(&self) -> Result<Vec<Schedule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, cron_expr, project, assigned_agent, skill, model, payload,
                    priority, enabled,
                    CAST(next_run_at AS VARCHAR), CAST(last_fired_at AS VARCHAR),
                    fire_count, CAST(created_at AS VARCHAR)
             FROM schedules
             WHERE enabled = TRUE AND next_run_at <= now()
             ORDER BY next_run_at ASC",
        )?;
        let rows = stmt
            .query_map([], map_schedule)?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    /// Mark a schedule fired and advance its `next_run_at`. The new
    /// time is computed by the caller (we don't keep cron-parsing
    /// dependencies in mamba-lake).
    pub fn advance_schedule(
        &self,
        id: &str,
        next_run_at: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE schedules
             SET next_run_at=?, last_fired_at=now(), fire_count=fire_count+1
             WHERE id=?",
            duckdb::params![next_run_at.to_rfc3339(), id],
        )?;
        Ok(())
    }
}

// ── Row mappers ───────────────────────────────────────────────────────────────

fn map_webhook(r: &duckdb::Row) -> duckdb::Result<Webhook> {
    Ok(Webhook {
        hook_id:          r.get(0)?,
        project:          r.get(1)?,
        assigned_agent:   r.get(2)?,
        skill:            r.get(3)?,
        model:            r.get(4)?,
        payload_template: r.get(5)?,
        priority:         r.get(6)?,
        enabled:          r.get(7)?,
        created_at:       parse_ts(r.get::<_, String>(8)?)?,
        last_fired_at:    r.get::<_, Option<String>>(9)?
                              .map(parse_ts).transpose()?,
        fire_count:       r.get::<_, u64>(10)?,
    })
}

fn map_schedule(r: &duckdb::Row) -> duckdb::Result<Schedule> {
    Ok(Schedule {
        id:               r.get(0)?,
        cron_expr:        r.get(1)?,
        project:          r.get(2)?,
        assigned_agent:   r.get(3)?,
        skill:            r.get(4)?,
        model:            r.get(5)?,
        payload:          r.get(6)?,
        priority:         r.get(7)?,
        enabled:          r.get(8)?,
        next_run_at:      parse_ts(r.get::<_, String>(9)?)?,
        last_fired_at:    r.get::<_, Option<String>>(10)?
                              .map(parse_ts).transpose()?,
        fire_count:       r.get::<_, u64>(11)?,
        created_at:       parse_ts(r.get::<_, String>(12)?)?,
    })
}

fn parse_ts(s: String) -> duckdb::Result<DateTime<Utc>> {
    // DuckDB casts timestamps to strings as `2026-04-27 20:40:32.123456+00`,
    // which isn't strict RFC3339 (space instead of `T`, no `:` in tz). We
    // accept both shapes — RFC3339 first (in case some path inserted that),
    // then DuckDB's native form.
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let normalized = s.replace(' ', "T");
    // Append the missing colon in the timezone offset (`+00` -> `+00:00`).
    let normalized = if normalized.ends_with("+00") || normalized.ends_with("-00") {
        format!("{normalized}:00")
    } else if normalized.len() >= 3 {
        let (head, tail) = normalized.split_at(normalized.len() - 3);
        // tail may already be like `+00` or like `:00`.
        if (tail.starts_with('+') || tail.starts_with('-')) && !tail.contains(':') {
            format!("{head}{tail}:00")
        } else {
            normalized.clone()
        }
    } else {
        normalized.clone()
    };
    DateTime::parse_from_rfc3339(&normalized)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            duckdb::Error::FromSqlConversionFailure(
                0,
                duckdb::types::Type::Any,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad timestamp {s:?} -> {normalized:?}: {e}"),
                )),
            )
        })
}
