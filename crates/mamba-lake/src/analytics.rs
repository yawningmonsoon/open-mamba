use crate::store::Lake;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Aggregate token + cost across ALL agents, projects, models.
/// This is the "total burn" view.
#[derive(Debug, Serialize, Deserialize)]
pub struct GlobalConsumption {
    pub total_tasks: i64,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectConsumption {
    pub project: String,
    pub tasks: i64,
    pub tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentConsumption {
    pub agent_slug: String,
    pub model: String,
    pub tasks: i64,
    pub tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DailyBurn {
    pub day: String,
    pub tokens: i64,
    pub cost_usd: f64,
    pub tasks: i64,
}

impl Lake {
    pub fn global_consumption(&self) -> Result<GlobalConsumption> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT COUNT(*), COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0),
                    COALESCE(SUM(tokens_in)+SUM(tokens_out),0), COALESCE(SUM(cost_usd),0)
             FROM task_envelopes WHERE status='done'"
        )?;
        let row = stmt.query_row([], |r| {
            Ok(GlobalConsumption {
                total_tasks: r.get(0)?,
                total_tokens_in: r.get(1)?,
                total_tokens_out: r.get(2)?,
                total_tokens: r.get(3)?,
                total_cost_usd: r.get(4)?,
            })
        })?;
        Ok(row)
    }

    pub fn per_project_consumption(&self) -> Result<Vec<ProjectConsumption>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT project, COUNT(*), COALESCE(SUM(tokens_in+tokens_out),0), COALESCE(SUM(cost_usd),0)
             FROM task_envelopes WHERE status='done'
             GROUP BY project ORDER BY cost_usd DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectConsumption {
                project: r.get(0)?,
                tasks: r.get(1)?,
                tokens: r.get(2)?,
                cost_usd: r.get(3)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
        Ok(rows)
    }

    pub fn per_agent_consumption(&self) -> Result<Vec<AgentConsumption>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT assigned_agent, model, COUNT(*),
                    COALESCE(SUM(tokens_in+tokens_out),0), COALESCE(SUM(cost_usd),0)
             FROM task_envelopes WHERE status='done'
             GROUP BY assigned_agent, model ORDER BY cost_usd DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AgentConsumption {
                agent_slug: r.get(0)?,
                model: r.get(1)?,
                tasks: r.get(2)?,
                tokens: r.get(3)?,
                cost_usd: r.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
        Ok(rows)
    }

    pub fn daily_burn_last_30(&self) -> Result<Vec<DailyBurn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT strftime(completed_at, '%Y-%m-%d') AS day,
                    COALESCE(SUM(tokens_in+tokens_out),0),
                    COALESCE(SUM(cost_usd),0),
                    COUNT(*)
             FROM task_envelopes
             WHERE status='done' AND completed_at >= now() - INTERVAL 30 DAYS
             GROUP BY day ORDER BY day DESC"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(DailyBurn {
                day: r.get(0)?,
                tokens: r.get(1)?,
                cost_usd: r.get(2)?,
                tasks: r.get(3)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
        Ok(rows)
    }
}
